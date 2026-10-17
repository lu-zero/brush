//! Facilities for implementing and managing builtins

pub use futures::future::BoxFuture;
use std::any::{Any, TypeId, type_name};
use std::io::Write;
use std::marker::PhantomData;

pub mod args;

use crate::{BuiltinError, CommandArg, commands, error, extensions, results};
pub use args::{ArgsError, FromArgs};

/// A type-erased, cloneable container for per-builtin state stored on the shell.
///
/// Any `T: Clone + Send + Sync + 'static` automatically implements this trait
/// thanks to a blanket impl.
///
/// # Important: calling methods on `Box<dyn AnyState>`
///
/// Because `Box<dyn AnyState>` itself satisfies `Clone + Send + Sync + 'static`,
/// the blanket impl also applies to it. When calling `as_any`, `as_any_mut`, or
/// `clone_box` on a `Box<dyn AnyState>`, you **must** explicitly dereference
/// first (e.g. `(&**state).as_any()`) so that dispatch goes through the vtable to
/// the concrete inner type, rather than the blanket impl on `Box<dyn AnyState>`
/// itself. The accessors on [`Shell`](crate::Shell) and
/// [`ExecutionContext`](crate::commands::ExecutionContext) already handle this
/// correctly.
pub trait AnyState: Send + Sync + 'static {
    /// Deep-clone the state into a new heap allocation.
    fn clone_box(&self) -> Box<dyn AnyState>;

    /// Downcast to `&dyn Any` for typed access.
    fn as_any(&self) -> &dyn Any;

    /// Downcast to `&mut dyn Any` for typed mutable access.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Clone + Send + Sync + 'static> AnyState for T {
    fn clone_box(&self) -> Box<dyn AnyState> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Clone for Box<dyn AnyState> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

/// Marker type indicating that a [`Registration`] has not yet been given a
/// custom local-state override via [`Registration::with_state`].
///
/// The phantom parameter `St` carries the builtin's `State` type so that
/// `with_state(state)` can enforce the correct argument type at compile time.
pub struct NeedsLocalState<St>(PhantomData<St>);

/// Marker type indicating that a [`Registration`] is in its storage/terminal
/// form.
///
/// Either local state was provided via [`Registration::with_state`], or the
/// registration was produced by [`simple_builtin`] (which has no local state
/// concept). In this state, [`with_state`](Registration::with_state) is not
/// available, preventing double-provision.
pub struct HasLocalState;

/// Type of a function implementing a built-in command.
///
/// # Arguments
///
/// * The context in which the command is being executed. The name the built-in
///   was invoked under is `context.command_name`.
/// * The arguments following that name. The name is *not* repeated here.
#[allow(type_alias_bounds)]
pub type CommandExecuteFunc<SE: extensions::ShellExtensions> =
    fn(
        commands::ExecutionContext<'_, SE>,
        Vec<commands::CommandArg>,
    ) -> BoxFuture<'_, Result<results::ExecutionResult, error::Error>>;

/// Trait implemented by built-in shell commands.
pub trait Command: FromArgs + HelpContent {
    /// The error type returned by the command. See [`Command::execute`] for when
    /// to return an error rather than a failing [`results::ExecutionResult`].
    type Error: BuiltinError + 'static;

    /// The type of persistent state carried by this builtin across invocations.
    ///
    /// Stateful builtins override this with a custom type that implements
    /// `Clone + Default + Send + Sync + 'static`. The shell allocates a default
    /// instance at registration time and stores it keyed by the builtin's
    /// registered name. Builtins access state through
    /// [`ExecutionContext::builtin_state_mut`] and external code uses
    /// [`Shell::builtin_state_of`] / [`Shell::builtin_state_mut_of`].
    type State: Clone + Default + Send + Sync + 'static;

    /// The type of shared state that this builtin accesses in coordination with
    /// other builtins registered through the same [`SharedBuilder`].
    ///
    /// Unlike [`State`](Command::State), shared state is **not** per-builtin —
    /// it is keyed by type (`TypeId`) and shared across all builtins registered
    /// through the same builder. The default `()` means "no shared state".
    ///
    /// Use `Arc<T>` when state should survive `Shell::clone()` (subshells
    /// share the same underlying data). Use a bare `T` when each subshell
    /// should get an independent copy (via `T::clone`).
    ///
    /// To mutate shared state, `T` must provide interior mutability (e.g.
    /// `Mutex`, `papaya::HashMap`, atomics).
    type SharedState: Clone + Send + Sync + 'static;

    /// Returns a shared reference to this builtin's persistent state.
    ///
    /// This is a convenience wrapper around
    /// [`ExecutionContext::builtin_state`] that infers the command type
    /// from `&self`, so no turbofish is needed.
    fn state<'a, SE: extensions::ShellExtensions>(
        &self,
        context: &'a commands::ExecutionContext<'_, SE>,
    ) -> Result<&'a Self::State, error::Error> {
        context.builtin_state::<Self>()
    }

    /// Returns an exclusive reference to this builtin's persistent state.
    ///
    /// This is a convenience wrapper around
    /// [`ExecutionContext::builtin_state_mut`] that infers the command type
    /// from `&self`, so no turbofish is needed.
    ///
    /// The caller must drop the returned reference before calling any other
    /// `&mut Shell` method (including `source_script`), so that re-entrant
    /// builtin invocations can access state independently.
    fn state_mut<'a, SE: extensions::ShellExtensions>(
        &self,
        context: &'a mut commands::ExecutionContext<'_, SE>,
    ) -> Result<&'a mut Self::State, error::Error> {
        context.builtin_state_mut::<Self>()
    }

    /// Returns a shared reference to this builtin's shared state.
    ///
    /// This is a convenience wrapper around
    /// [`ExecutionContext::shared`] that infers the shared-state type
    /// from `Self::SharedState`, so no turbofish is needed.
    fn shared<'a, SE: extensions::ShellExtensions>(
        &self,
        context: &'a commands::ExecutionContext<'_, SE>,
    ) -> Result<&'a Self::SharedState, error::Error> {
        context.shared::<Self::SharedState>()
    }

    /// Executes the built-in command in the provided context.
    ///
    /// # Reporting failure
    ///
    /// Argument errors from [`FromArgs`] are reported by the shell before this
    /// is called. For failures found while executing, a builtin can either:
    ///
    /// * **Return a failing exit status.** Write any diagnostic to
    ///   `context.stderr()` in the builtin's own words, then return `Ok` with a
    ///   non-zero status. The shell uses the status as-is.
    /// * **Return an error.** The shell prints it through its error formatter,
    ///   attributed to the builtin by name, and takes the exit status from the
    ///   error's exit-code conversion. For a special builtin in POSIX mode, an
    ///   error is fatal in a non-interactive shell. An I/O error for a broken
    ///   pipe is the exception: it is never printed or fatal, and just sets the
    ///   status.
    ///
    /// A successful result can also redirect control flow through its
    /// `next_control_flow` field, as `break`, `return`, and `exit` do.
    ///
    /// # Arguments
    ///
    /// * `context` - The context in which the command is being executed.
    ///
    /// # Errors
    ///
    /// Returns an error for a failure the shell should report on the builtin's
    /// behalf; see "Reporting failure" above.
    // NOTE: we use desugared async here because we need a Send marker
    fn execute<SE: extensions::ShellExtensions>(
        &self,
        context: commands::ExecutionContext<'_, SE>,
    ) -> impl std::future::Future<Output = Result<results::ExecutionResult, Self::Error>>
    + std::marker::Send;
}

/// Trait yielding a built-in command's help text.
///
/// The short forms are structured: `help -s` and `help -d` frame them
/// uniformly (as `name: synopsis` and `name - description`), so implementations
/// return only the part that differs. Detailed help is free-form and printed
/// verbatim.
pub trait HelpContent {
    /// Returns the command's synopsis, e.g. `cd [-L|-P] [dir]`. Includes the
    /// command's own name, but no `name: ` prefix and no trailing newline.
    ///
    /// # Arguments
    ///
    /// * `name` - The name the built-in was invoked under.
    fn synopsis(name: &str) -> String;

    /// Returns a one-line description of the command, with no `name - ` prefix
    /// and no trailing newline.
    ///
    /// # Arguments
    ///
    /// * `name` - The name the built-in was invoked under.
    fn description(name: &str) -> String;

    /// Returns the command's full help, rendered however the implementation
    /// likes. Defaults to the synopsis followed by the indented description.
    ///
    /// # Arguments
    ///
    /// * `name` - The name the built-in was invoked under.
    /// * `options` - Options controlling how the content is rendered.
    ///
    /// # Errors
    ///
    /// Returns an error if the help cannot be rendered.
    fn detailed_help(name: &str, _options: &ContentOptions) -> Result<String, error::Error> {
        Ok(format!(
            "{name}: {}\n    {}\n",
            Self::synopsis(name),
            Self::description(name)
        ))
    }
}

/// Options for retrieving built-in command content.
///
/// Non-exhaustive so options can be added without breaking implementors:
/// construct with [`ContentOptions::default`] and set the fields you need.
#[derive(Default)]
#[non_exhaustive]
pub struct ContentOptions {
    /// Whether or not the content should be colorized.
    pub colorized: bool,
}

/// Encapsulates a registration for a built-in command.
///
/// Construct one with [`builtin`] for a [`Command`] implementation, or with
/// [`Registration::new`] for a bare function. The execution function and whether
/// the builtin takes declarations are fixed at construction, because the shell
/// relies on the two agreeing.
///
/// # Type parameters
///
/// * `SE` — the [`ShellExtensions`](extensions::ShellExtensions) type.
/// * `S`  — the **shared-state** phantom. `S = ()` (default) means this registration can be passed
///   directly to [`Shell::register_builtin`](crate::Shell::register_builtin). Any other `S` (e.g.
///   `Arc<RepoCache>`) means it must go through [`SharedBuilder`] or [`SharedHandle`].
/// * `L`  — the **local-state** phantom, governing [`with_state`](Registration::with_state)
///   availability:
///   - [`NeedsLocalState<St>`] — `with_state` is available, takes `St`.
///   - [`HasLocalState`] — `with_state` is not available (terminal/storage form).
///
/// # Stored form
///
/// `Registration<SE>` (using both defaults) is the **stored form** used by
/// `Shell.builtins` and `ShellBuilder`. It is produced by calling
/// [`into_storage`](Registration::into_storage) on a freshly-created
/// registration.
pub struct Registration<SE: extensions::ShellExtensions, S = (), L = HasLocalState> {
    /// Function to execute the builtin.
    execute_func: CommandExecuteFunc<SE>,

    /// Functions yielding the builtin's help text; see [`HelpContent`].
    synopsis_func: fn(&str) -> String,
    description_func: fn(&str) -> String,
    detailed_help_func: fn(&str, &ContentOptions) -> Result<String, error::Error>,

    /// Has this registration been disabled?
    disabled: bool,

    /// Is the builtin classified as "special" by specification?
    special: bool,

    /// Whether the builtin takes specially handled declarations. Read during
    /// word expansion, before the builtin runs, to decide whether `name=value`
    /// words become [`CommandArg::Assignment`]; `execute_func` must expect that.
    takes_declarations: bool,

    /// Factory function that creates the default state for this builtin.
    /// Called by [`Shell::register_builtin`](crate::Shell::register_builtin)
    /// to seed the per-builtin state map.
    pub state_init: fn() -> Box<dyn AnyState>,

    /// The builtin's state: the one set by [`with_state`](Registration::with_state),
    /// or, once registered, the default one. `None` until then, unless it was set.
    pub state: Option<Box<dyn AnyState>>,

    /// Shared-state phantom (`()` for stored form).
    pub _shared: PhantomData<S>,
    /// Local-state phantom (`HasLocalState` for stored form).
    pub _local: PhantomData<L>,
}

impl<SE: extensions::ShellExtensions, S, L> Clone for Registration<SE, S, L> {
    fn clone(&self) -> Self {
        Self {
            execute_func: self.execute_func,
            synopsis_func: self.synopsis_func,
            description_func: self.description_func,
            detailed_help_func: self.detailed_help_func,
            disabled: self.disabled,
            special: self.special,
            takes_declarations: self.takes_declarations,
            state_init: self.state_init,
            state: self.state.clone(),
            _shared: PhantomData,
            _local: PhantomData,
        }
    }
}

impl<SE: extensions::ShellExtensions, S, L> Registration<SE, S, L> {
    /// Returns a registration for a builtin implemented directly as a function,
    /// rather than through [`Command`]. The builtin receives its arguments as
    /// plain strings; it never takes declarations.
    ///
    /// # Arguments
    ///
    /// * `execute_func` - Function to execute the builtin.
    pub const fn new<H: HelpContent>(execute_func: CommandExecuteFunc<SE>) -> Self {
        Self {
            execute_func,
            synopsis_func: H::synopsis,
            description_func: H::description,
            detailed_help_func: H::detailed_help,
            disabled: false,
            special: false,
            takes_declarations: false,
            state_init: || Box::new(()),
            state: None,
            _shared: PhantomData,
            _local: PhantomData,
        }
    }

    /// Returns the function that executes the builtin.
    pub const fn execute_func(&self) -> CommandExecuteFunc<SE> {
        self.execute_func
    }

    /// Returns the builtin's synopsis; see [`HelpContent::synopsis`].
    ///
    /// # Arguments
    ///
    /// * `name` - The name the builtin was invoked under.
    pub fn synopsis(&self, name: &str) -> String {
        (self.synopsis_func)(name)
    }

    /// Returns the builtin's one-line description; see
    /// [`HelpContent::description`].
    ///
    /// # Arguments
    ///
    /// * `name` - The name the builtin was invoked under.
    pub fn description(&self, name: &str) -> String {
        (self.description_func)(name)
    }

    /// Returns the builtin's full help; see [`HelpContent::detailed_help`].
    ///
    /// # Arguments
    ///
    /// * `name` - The name the builtin was invoked under.
    /// * `options` - Options controlling how the content is rendered.
    ///
    /// # Errors
    ///
    /// Returns an error if the help cannot be rendered.
    pub fn detailed_help(
        &self,
        name: &str,
        options: &ContentOptions,
    ) -> Result<String, error::Error> {
        (self.detailed_help_func)(name, options)
    }

    /// Returns whether the builtin takes specially handled declarations.
    pub const fn takes_declarations(&self) -> bool {
        self.takes_declarations
    }

    /// Returns whether the registration has been disabled (see `enable`).
    pub const fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Enables or disables the registration.
    ///
    /// # Arguments
    ///
    /// * `disabled` - Whether the builtin should be disabled.
    pub const fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    /// Returns whether the builtin is classified as "special" by specification.
    pub const fn is_special(&self) -> bool {
        self.special
    }

    /// Updates the given registration to mark it for a special builtin.
    #[must_use]
    pub fn special(self) -> Self {
        Self {
            special: true,
            ..self
        }
    }

    /// Convert to the stored form (`Registration<SE, (), HasLocalState>`).
    ///
    /// This erases the shared-state phantom and transitions the local-state
    /// phantom to [`HasLocalState`], while preserving any
    /// [`with_state`](Registration::with_state) override in
    /// `state`.
    ///
    /// Called by [`stored_builtin`], [`stored_decl_builtin`],
    /// [`stored_raw_arg_builtin`], and
    /// [`ShellBuilder::builtin`](crate::ShellBuilder::builtin).
    pub(crate) fn into_storage(self) -> Registration<SE> {
        Registration {
            execute_func: self.execute_func,
            synopsis_func: self.synopsis_func,
            description_func: self.description_func,
            detailed_help_func: self.detailed_help_func,
            disabled: self.disabled,
            special: self.special,
            takes_declarations: self.takes_declarations,
            state_init: self.state_init,
            state: self.state,
            _shared: PhantomData,
            _local: PhantomData,
        }
    }

    /// Convert to the stored form, with the state given to
    /// [`with_state`](Registration::with_state) or else the default one.
    ///
    /// For internal use by registration methods in `Shell` and
    /// [`SharedBuilder`]/[`SharedHandle`].
    pub(crate) fn into_registered(self) -> Registration<SE> {
        let state_init = self.state_init;
        let mut stored = self.into_storage();
        stored.state.get_or_insert_with(state_init);
        stored
    }

    /// Erase the local-state phantom to [`HasLocalState`], preserving the
    /// shared-state phantom `S`. Used by [`SharedBuilder`] and
    /// [`SharedHandle`], which accept registrations in any local-state
    /// typestate and store them in a uniform form.
    pub(crate) fn normalize_local(self) -> Registration<SE, S, HasLocalState> {
        Registration {
            execute_func: self.execute_func,
            synopsis_func: self.synopsis_func,
            description_func: self.description_func,
            detailed_help_func: self.detailed_help_func,
            disabled: self.disabled,
            special: self.special,
            takes_declarations: self.takes_declarations,
            state_init: self.state_init,
            state: self.state,
            _shared: PhantomData,
            _local: PhantomData,
        }
    }
}

impl<SE: extensions::ShellExtensions, S, St: Clone + Send + Sync + 'static>
    Registration<SE, S, NeedsLocalState<St>>
{
    /// Provide a custom initial value for this builtin's local state,
    /// replacing the default produced by `B::State::default()`.
    ///
    /// The argument type is exactly `St` (= `B::State`), enforced at compile
    /// time by the [`NeedsLocalState<St>`] phantom.
    ///
    /// This method is only available once. After calling it the registration
    /// transitions to [`HasLocalState`] and `with_state` is no longer
    /// available.
    #[must_use]
    pub fn with_state(self, state: St) -> Registration<SE, S, HasLocalState> {
        Registration {
            execute_func: self.execute_func,
            synopsis_func: self.synopsis_func,
            description_func: self.description_func,
            detailed_help_func: self.detailed_help_func,
            disabled: self.disabled,
            special: self.special,
            takes_declarations: self.takes_declarations,
            state_init: self.state_init,
            state: Some(Box::new(state)),
            _shared: self._shared,
            _local: PhantomData,
        }
    }
}

/// Returns a built-in command registration, given an implementation of the
/// `Command` trait.
///
/// The phantom types encode:
/// * `S = B::SharedState` — gates which registration method can be used.
/// * `L = NeedsLocalState<B::State>` — enables [`with_state`](Registration::with_state).
pub fn builtin<B: Command + Send + Sync, SE: extensions::ShellExtensions>()
-> Registration<SE, B::SharedState, NeedsLocalState<B::State>> {
    Registration {
        takes_declarations: B::TAKES_DECLARATIONS,
        state_init: default_state_fn::<B::State>,
        ..Registration::new::<B>(exec_builtin::<B, SE>)
    }
}

fn default_state_fn<S: Clone + Default + Send + Sync + 'static>() -> Box<dyn AnyState> {
    Box::new(S::default())
}

fn exec_builtin<T: Command + Send + Sync, SE: extensions::ShellExtensions>(
    context: commands::ExecutionContext<'_, SE>,
    args: Vec<CommandArg>,
) -> BoxFuture<'_, Result<results::ExecutionResult, error::Error>> {
    Box::pin(async move { exec_builtin_impl::<T, SE>(context, args).await })
}

async fn exec_builtin_impl<T: Command + Send + Sync, SE: extensions::ShellExtensions>(
    context: commands::ExecutionContext<'_, SE>,
    args: Vec<CommandArg>,
) -> Result<results::ExecutionResult, error::Error> {
    let command = match T::from_args(context.command_name.as_str(), args) {
        Ok(command) => command,
        Err(e) => return Ok(report_args_error(&context, &e)),
    };

    call_builtin(command, context).await
}

/// Reports an argument-parsing outcome that stopped the built-in from running,
/// returning the result to yield: the message on stderr, and the usage status.
fn report_args_error<SE: extensions::ShellExtensions>(
    context: &commands::ExecutionContext<'_, SE>,
    error: &ArgsError,
) -> results::ExecutionResult {
    let _ = writeln!(context.stderr(), "{error}");
    results::ExecutionExitCode::InvalidUsage.into()
}

async fn call_builtin(
    command: impl Command,
    context: commands::ExecutionContext<'_, impl extensions::ShellExtensions>,
) -> Result<results::ExecutionResult, error::Error> {
    let builtin_name = context.command_name.clone();
    let result = command
        .execute(context)
        .await
        .map_err(|e| error::ErrorKind::BuiltinError(Box::new(e), builtin_name))?;

    Ok(result)
}

/// Consuming builder that registers multiple builtins sharing a single
/// typed state value.
///
/// # Subshell cloning behaviour
///
/// When [`Shell::clone()`](crate::Shell::clone) is called, shared state is
/// cloned via [`AnyState::clone_box`]. With `Arc<T>` the clone is a cheap
/// refcount bump (all subshells see the same data). With a bare `T` each
/// subshell gets an independent deep copy.
///
/// # Interior mutability
///
/// The accessor [`ExecutionContext::shared`] returns `&T`. To mutate through
/// an `Arc<T>`, `T` itself must provide interior mutability (e.g. `Mutex`,
/// `papaya::HashMap`, atomics).
///
/// # Type uniqueness
///
/// Shared state is keyed by [`TypeId`]. Two unrelated uses of the same
/// generic type (e.g. `HashMap<String, String>`) would collide. Use newtype
/// wrappers for isolation.
///
/// # Example
///
/// ```ignore
/// let cache = SharedBuilder::new(Arc::new(RepoCache::default()))
///     .builtin("inherit", builtin::<InheritCommand, _>());
/// shell.register_shared(cache);
/// ```
pub struct SharedBuilder<T, SE: extensions::ShellExtensions = extensions::DefaultShellExtensions> {
    /// The shared state value to be seeded into `Shell::shared_states`.
    pub(crate) value: T,
    /// Builtins to register alongside the shared state.
    pub(crate) builtins: Vec<(String, Registration<SE, T, HasLocalState>)>,
}

impl<T: Clone + Send + Sync + 'static, SE: extensions::ShellExtensions> SharedBuilder<T, SE> {
    /// Create a new builder that will share `value` across all added builtins.
    pub const fn new(value: T) -> Self {
        Self {
            value,
            builtins: Vec::new(),
        }
    }

    /// Add a builtin that shares state type `T`.
    ///
    /// Compile error if the registration's shared-state phantom is not `T`.
    /// The registration may be in any local-state typestate (e.g. as produced
    /// by [`builtin`] before calling [`with_state`](Registration::with_state)).
    #[must_use]
    pub fn builtin<L>(mut self, name: impl Into<String>, reg: Registration<SE, T, L>) -> Self {
        self.builtins.push((name.into(), reg.normalize_local()));
        self
    }
}

/// Borrowing handle that registers builtins against an **existing** shared
/// state entry on a [`Shell`](crate::Shell).
///
/// Obtained via [`Shell::shared_handle`](crate::Shell::shared_handle).
/// Each call to [`builtin`](SharedHandle::builtin) registers immediately.
pub struct SharedHandle<'a, T, SE: extensions::ShellExtensions> {
    pub(crate) shell: &'a mut crate::Shell<SE>,
    pub(crate) _phantom: PhantomData<T>,
}

impl<T: Clone + Send + Sync + 'static, SE: extensions::ShellExtensions> SharedHandle<'_, T, SE> {
    /// Register a builtin against the existing shared state of type `T`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::SharedStateNotRegistered`](crate::error::ErrorKind::SharedStateNotRegistered)
    /// if the shared state has not been seeded (i.e. if
    /// [`register_shared`](crate::Shell::register_shared) or
    /// [`set_shared`](crate::Shell::set_shared) has not been called for `T`).
    ///
    /// The registration may be in any local-state typestate (e.g. as produced
    /// by [`builtin`] before calling [`with_state`](Registration::with_state)).
    pub fn builtin<L>(
        &mut self,
        name: impl Into<String>,
        reg: Registration<SE, T, L>,
    ) -> Result<(), error::Error> {
        if !self.shell.shared_states().contains_key(&TypeId::of::<T>()) {
            return Err(error::Error::from(
                error::ErrorKind::SharedStateNotRegistered(type_name::<T>().to_string()),
            ));
        }
        self.shell
            .builtins
            .insert(name.into(), reg.into_registered());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    struct Counter {
        value: usize,
    }

    #[test]
    fn any_state_clone_roundtrips() {
        let original: Box<dyn AnyState> = Box::new(Counter { value: 42 });
        let cloned = original.clone();
        let downcasted = (*cloned).as_any().downcast_ref::<Counter>().unwrap();
        assert_eq!(downcasted.value, 42);
    }

    #[test]
    fn any_state_mut_roundtrip() {
        let mut state: Box<dyn AnyState> = Box::new(Counter { value: 0 });
        (*state)
            .as_any_mut()
            .downcast_mut::<Counter>()
            .unwrap()
            .value += 1;
        assert_eq!(
            (*state).as_any().downcast_ref::<Counter>().unwrap().value,
            1
        );
    }

    #[test]
    fn any_state_wrong_type_returns_none() {
        let state: Box<dyn AnyState> = Box::new(Counter { value: 1 });
        assert!((*state).as_any().downcast_ref::<String>().is_none());
    }

    #[test]
    fn any_state_as_any_mut_downcast() {
        let mut state: Box<dyn AnyState> = Box::new(Counter { value: 5 });
        let c = (*state).as_any_mut().downcast_mut::<Counter>();
        assert!(c.is_some(), "downcast_mut to Counter should succeed");
        assert_eq!(c.unwrap().value, 5);
    }

    #[test]
    fn any_state_complex_type() {
        let mut state: Box<dyn AnyState> = Box::new(Counter { value: 5 });
        (*state)
            .as_any_mut()
            .downcast_mut::<Counter>()
            .unwrap()
            .value += 1;
        assert_eq!(
            (*state).as_any().downcast_ref::<Counter>().unwrap().value,
            6
        );
    }
}
