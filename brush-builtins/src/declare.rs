use itertools::Itertools;
use std::{io::Write, sync::LazyLock};

use brush_core::{
    ErrorKind, ExecutionResult, builtins,
    env::{self, EnvironmentLookup, EnvironmentScope, VarNameExt},
    parser::ast,
    variables::{
        self, ArrayLiteral, ShellValue, ShellValueLiteral, ShellValueUnsetType, ShellVariable,
        ShellVariableUpdateTransform,
    },
};

/// Display or update variables and their attributes.
#[derive(Default, winnow_args::Args)]
#[arg(
    usage = "declare [OPTIONS] [DECLARATIONS]...",
    plus_options,
    disable_help_short,
    disable_version_flag,
    disable_help_subcommand
)]
pub(crate) struct DeclareCommand {
    /// Constrain to function names or definitions.
    #[arg(short = 'f')]
    function_names_or_defs_only: bool,

    /// Constrain to function names only.
    #[arg(short = 'F')]
    function_names_only: bool,

    /// Create global variable, if applicable.
    #[arg(short = 'g')]
    create_global: bool,

    /// When creating a local variable that shadows another variable of the same name,
    /// then initialize it with the contents and attributes of the variable being shadowed.
    #[arg(short = 'I')]
    locals_inherit_from_prev_scope: bool,

    /// Display each item's attributes and values.
    #[arg(short = 'p')]
    print: bool,

    /// Make the variable an indexed array.
    #[arg(short = 'a', plus = 'a')]
    make_indexed_array: Vec<bool>,

    /// Make the variable an associative array.
    #[arg(short = 'A', plus = 'A')]
    make_associative_array: Vec<bool>,

    /// Enable capitalize-on-assignment for the variable.
    #[arg(short = 'c', plus = 'c')]
    capitalize_value_on_assignment: Vec<bool>,

    /// Mark the variable as integer-typed.
    #[arg(short = 'i', plus = 'i')]
    make_integer: Vec<bool>,

    /// Enable lowercase-on-assignment for the variable.
    #[arg(short = 'l', plus = 'l')]
    lowercase_value_on_assignment: Vec<bool>,

    /// Mark the variable as a name reference.
    #[arg(short = 'n', plus = 'n')]
    make_nameref: Vec<bool>,

    /// Mark the variable as read-only.
    #[arg(short = 'r', plus = 'r')]
    make_readonly: Vec<bool>,

    /// Enable tracing for the variable.
    #[arg(short = 't', plus = 't')]
    make_traced: Vec<bool>,

    /// Enable uppercase-on-assignment for the variable.
    #[arg(short = 'u', plus = 'u')]
    uppercase_value_on_assignment: Vec<bool>,

    /// Mark the variable for export.
    #[arg(short = 'x', plus = 'x')]
    make_exported: Vec<bool>,

    //
    // Declarations
    //
    // N.B. These are skipped by winnow-args, but filled in by `impl_parse_args!`.
    #[arg(skip)]
    declarations: Vec<brush_core::CommandArg>,
}

brush_winnow_args_adapter::impl_parse_args!(DeclareCommand, declarations = declarations);

/// The change an attribute's `-X` (`true`) and `+X` (`false`) options make:
/// `+X` wins wherever it is given.
fn change(options: &[bool]) -> Option<bool> {
    if options.contains(&false) {
        Some(false)
    } else if options.contains(&true) {
        Some(true)
    } else {
        None
    }
}

/// What became of an operand that may assign more than the parser saw.
enum Lifted {
    /// The operand assigns this.
    Assignment(ast::Assignment),
    /// The operand is taken as it is.
    Unchanged,
    /// The operand assigns an array value that doesn't parse; that's been reported. As
    /// in bash, the array is still declared, and assigned no elements.
    Malformed(ast::Assignment),
}

#[derive(Clone, Copy)]
enum DeclareVerb {
    Declare,
    Local,
    Readonly,
}

impl<SE: brush_core::ShellExtensions> builtins::Builtin<SE> for DeclareCommand {
    type Error = brush_core::Error;

    async fn execute(
        self,
        mut context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        let verb = match context.command_name.as_str() {
            "local" => DeclareVerb::Local,
            "readonly" => DeclareVerb::Readonly,
            _ => DeclareVerb::Declare,
        };

        if matches!(verb, DeclareVerb::Local) && !context.shell.in_function() {
            let mut stderr_output = Vec::new();
            writeln!(stderr_output, "can only be used in a function")?;
            context.stderr().write_all(&stderr_output)?;
            context.stderr().flush()?;
            return Ok(ExecutionResult::general_error());
        }

        let mut output = Vec::new();
        let mut stderr_output = Vec::new();
        let mut result = ExecutionResult::success();

        if !self.declarations.is_empty() {
            for declaration in &self.declarations {
                if self.print && !matches!(verb, DeclareVerb::Readonly) {
                    if !self.try_display_declaration(
                        &context,
                        declaration,
                        verb,
                        &mut output,
                        &mut stderr_output,
                    )? {
                        result = ExecutionResult::general_error();
                    }
                } else {
                    let lifted;
                    let declaration = match self
                        .lift_assignment(&mut context, declaration, verb)
                        .await?
                    {
                        Lifted::Assignment(assignment) => {
                            lifted = brush_core::CommandArg::Assignment(assignment);
                            &lifted
                        }
                        Lifted::Unchanged => declaration,
                        Lifted::Malformed(assignment) => {
                            result = ExecutionResult::general_error();
                            lifted = brush_core::CommandArg::Assignment(assignment);
                            &lifted
                        }
                    };

                    if !self.process_declaration(&mut context, declaration, verb)? {
                        result = ExecutionResult::general_error();
                    }
                }
            }
        } else {
            if !self.function_names_only && !self.function_names_or_defs_only {
                self.display_matching_env_declarations(&context, verb, &mut output)?;
            }

            if !matches!(verb, DeclareVerb::Local | DeclareVerb::Readonly)
                && (!self.print || self.function_names_only || self.function_names_or_defs_only)
            {
                self.display_matching_functions(&context, &mut output)?;
            }
        }

        if !output.is_empty() {
            if let Some(mut stdout) = context.stdout_async() {
                stdout.write_all(&output).await?;
                stdout.flush().await?;
            } else {
                context.stdout().write_all(&output)?;
                context.stdout().flush()?;
            }
        }

        if !stderr_output.is_empty() {
            context.stderr().write_all(&stderr_output)?;
            context.stderr().flush()?;
        }

        Ok(result)
    }
}

impl DeclareCommand {
    fn try_display_declaration(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        declaration: &brush_core::CommandArg,
        verb: DeclareVerb,
        output: &mut Vec<u8>,
        stderr_output: &mut Vec<u8>,
    ) -> Result<bool, brush_core::Error> {
        let name = match declaration {
            brush_core::CommandArg::String(s) => s,
            brush_core::CommandArg::Assignment(_) => {
                writeln!(stderr_output, "declare: {declaration}: not found")?;
                return Ok(false);
            }
        };

        let lookup = if matches!(verb, DeclareVerb::Local) {
            EnvironmentLookup::OnlyInCurrent(EnvironmentScope::Local)
        } else {
            EnvironmentLookup::Anywhere
        };

        if self.function_names_only || self.function_names_or_defs_only {
            if let Some(func_registration) = context.shell.funcs().get(name) {
                if self.function_names_only {
                    if self.print {
                        writeln!(output, "declare -f {name}")?;
                    } else {
                        writeln!(output, "{name}")?;
                    }
                } else {
                    writeln!(output, "{}", func_registration.definition())?;
                }
                Ok(true)
            } else {
                Ok(false)
            }
        } else if let Some((_, variable)) = context
            .shell
            .env()
            .lookup(name.as_str().direct())
            .in_scope(lookup)
            .get_direct()
        {
            let mut cs = variable.attribute_flags(context.shell);
            if cs.is_empty() {
                cs.push('-');
            }

            let resolved_value = variable.resolve_value(context.shell);
            let separator_str = if matches!(resolved_value, ShellValue::Unset(_)) {
                ""
            } else {
                "="
            };

            writeln!(
                output,
                "declare -{cs} {name}{separator_str}{}",
                resolved_value.format(variables::FormatStyle::DeclarePrint, context.shell)?
            )?;

            Ok(true)
        } else {
            writeln!(stderr_output, "declare: {name}: not found")?;
            Ok(false)
        }
    }

    /// `declare -f` with attribute flags applies them to the named function rather than
    /// displaying it (e.g. `declare -ft name`, `declare -fx name`).
    fn apply_function_attributes(
        &self,
        context: &mut brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        declaration: &brush_core::CommandArg,
    ) -> bool {
        let func = match declaration {
            brush_core::CommandArg::String(name) => context.shell.func_mut(name),
            brush_core::CommandArg::Assignment(_) => None,
        };

        // As with display, bash reports failure without printing an error message here.
        let Some(func) = func else {
            return false;
        };

        match change(&self.make_exported) {
            Some(true) => func.export(),
            Some(false) => func.unexport(),
            None => (),
        }

        // TODO(declare): function tracing (-t) isn't tracked; it's accepted silently.
        true
    }

    #[expect(clippy::too_many_lines)]
    fn process_declaration(
        &self,
        context: &mut brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        declaration: &brush_core::CommandArg,
        verb: DeclareVerb,
    ) -> Result<bool, brush_core::Error> {
        let create_var_local = self.creates_local(context, verb);

        if (self.function_names_or_defs_only || self.function_names_only)
            && (change(&self.make_traced).is_some() || change(&self.make_exported).is_some())
        {
            return Ok(self.apply_function_attributes(context, declaration));
        }

        if self.function_names_or_defs_only || self.function_names_only {
            let mut output = Vec::new();
            let mut stderr_output = Vec::new();
            let result = self.try_display_declaration(
                context,
                declaration,
                verb,
                &mut output,
                &mut stderr_output,
            )?;
            if !output.is_empty() {
                let _ = context.stdout().write_all(&output);
                let _ = context.stdout().flush();
            }
            if !stderr_output.is_empty() {
                let _ = context.stderr().write_all(&stderr_output);
                let _ = context.stderr().flush();
            }
            return Ok(result);
        }

        // Extract the variable name and the initial value being assigned (if any).
        let (name, assigned_index, initial_value, name_is_array, append) =
            Self::declaration_to_name_and_value(declaration)?;

        if name == "-" && matches!(verb, DeclareVerb::Local) {
            tracing::warn!("not yet implemented: local -");
            return Ok(true);
        }

        if !env::valid_variable_name(name.as_str()) {
            writeln!(
                context.stderr(),
                "{}: `{declaration}': not a valid identifier",
                context.command_name
            )?;
            return Ok(false);
        }

        // In bash, `declare -ni var=value` fails — the combination of nameref
        // and integer attributes with an initial value is rejected. Without a
        // value (e.g., `declare -ni var`), bash applies both attributes.
        let nameref_integer_conflict = matches!(change(&self.make_nameref), Some(true))
            && matches!(change(&self.make_integer), Some(true))
            && initial_value.is_some();
        if nameref_integer_conflict {
            return Ok(false);
        }

        // If the variable has (or is being given) the integer attribute,
        // scalar values are evaluated as arithmetic expressions — in bash,
        // `declare -i x=20+5` sets x to 25. Evaluate before borrowing the
        // environment mutably.
        let initial_value = match initial_value {
            Some(ShellValueLiteral::Scalar(expr)) => {
                let existing_is_integer_or_readonly =
                    context.shell.env().get(name.as_str()).map(|resolved| {
                        (
                            resolved.base_var().is_treated_as_integer(),
                            resolved.base_var().is_readonly(),
                        )
                    });
                let integer_now = match change(&self.make_integer) {
                    Some(explicit) => explicit,
                    None => existing_is_integer_or_readonly.is_some_and(|(is_int, _)| is_int),
                };
                // A readonly target rejects the assignment before its value is
                // examined, so the expression's side effects must not run either.
                let target_is_readonly = existing_is_integer_or_readonly.is_some_and(|(_, ro)| ro);
                if integer_now && !target_is_readonly {
                    let parsed = brush_parser::arithmetic::parse_with(
                        expr.as_str(),
                        context.shell.parser_options().parser_impl,
                    )?;
                    let result = brush_core::arithmetic::Evaluatable::eval(&parsed, context.shell)?;
                    Some(ShellValueLiteral::Scalar(result.to_string()))
                } else {
                    Some(ShellValueLiteral::Scalar(expr))
                }
            }
            other => other,
        };

        let lookup = Self::lookup(create_var_local);

        // The standalone `readonly` command rejects subscripted nameref targets
        // (e.g., `readonly ref` where ref→arr[1]). `declare -r` does NOT —
        // it applies the attribute to the base variable. This asymmetry
        // matches bash behavior.
        if matches!(verb, DeclareVerb::Readonly) {
            if let Ok(resolved) = context.shell.env().resolve_nameref(name.as_str()) {
                if resolved.subscript().is_some() {
                    let target = context
                        .shell
                        .env()
                        .resolve_nameref_to_name(name.as_str())
                        .unwrap_or_else(|_| name.clone());
                    writeln!(
                        context.stderr(),
                        "{}: `{target}': not a valid identifier",
                        context.command_name,
                    )?;
                    return Ok(false);
                }
            }
        }

        // Resolve namerefs for attribute-changing declarations and validate
        // any nameref target before modifying variable state.
        let (name, lookup) =
            self.resolve_nameref_for_declaration(context, name, lookup, create_var_local)?;

        let will_be_nameref = self.will_be_nameref();
        if will_be_nameref {
            if let Some(msg) =
                Self::validate_initial_nameref_target(name.as_str(), initial_value.as_ref())
            {
                writeln!(context.stderr(), "{}: {msg}", context.command_name)?;
                return Ok(false);
            }
        }

        // Look up the variable. Name is already resolved through
        // resolve_nameref_for_declaration above.
        let resolved_name = env::ResolvedName::already_resolved(name.as_str());

        // `local -I x[=v]` / `declare -I` (bash 5.0+): the new local inherits
        // value and attributes from the nearest same-name variable in an
        // enclosing scope instead of starting unset; `+=` appends to the
        // inherited value.
        if self.locals_inherit_from_prev_scope && create_var_local {
            let inherited = context
                .shell
                .env()
                .lookup(name.as_str())
                .in_scope(EnvironmentLookup::Anywhere)
                .get_direct()
                .map(|(_, var)| var.clone());

            if let Some(mut var) = inherited {
                self.apply_attributes_before_update(&mut var)?;

                if let Some(initial_value) = initial_value {
                    Self::assign_value(&mut var, initial_value, assigned_index, append)?;
                }

                if let Some(msg) = Self::validate_existing_nameref_value(&var) {
                    writeln!(context.stderr(), "{}: {msg}", context.command_name)?;
                    return Ok(false);
                }

                if context.shell.options().export_variables_on_modification
                    && !var.value().is_array()
                {
                    var.export();
                }

                self.apply_attributes_after_update(&mut var, verb)?;

                context
                    .shell
                    .env_mut()
                    .add(name, var, EnvironmentScope::Local)?;
                return Ok(true);
            }
            // No same-name variable anywhere: fall through to ordinary creation.
        }

        // If the variable currently holds a dynamic value, resolve its current
        // reading now, before taking a mutable borrow of the shell below. This
        // lets a scalar dynamic (e.g. RANDOM) that ends up getting materialized
        // by a shape conversion freeze at its actual current value instead of
        // an empty string.
        let resolved_dynamic_value = context
            .shell
            .env()
            .lookup(&resolved_name)
            .in_scope(lookup)
            .get_direct()
            .and_then(|(_, var)| {
                matches!(var.value(), brush_core::ShellValue::Dynamic { .. })
                    .then(|| var.resolve_value(context.shell))
            });

        // bash refuses to take the array attribute back off an existing array
        // ("cannot destroy array variables in this way") rather than quietly
        // turning it into a scalar, with or without a new value.
        if (change(&self.make_indexed_array) == Some(false)
            || change(&self.make_associative_array) == Some(false))
            && context
                .shell
                .env()
                .lookup(&resolved_name)
                .in_scope(lookup)
                .get_direct()
                .is_some_and(|(_, var)| var.value().is_array())
        {
            writeln!(
                context.stderr(),
                "{}: {name}: cannot destroy array variables in this way",
                context.command_name
            )?;
            return Ok(false);
        }

        // Look up the variable.
        if let Some((_, var)) = context
            .shell
            .env_mut()
            .lookup_mut(&resolved_name)
            .in_scope(lookup)
            .get_direct()
        {
            if change(&self.make_associative_array).is_some() {
                if initial_value.is_some() {
                    var.convert_to_associative_array_for_reassignment(
                        resolved_dynamic_value.as_ref(),
                    )?;
                } else {
                    var.convert_to_associative_array(resolved_dynamic_value.as_ref())?;
                }
            }
            if change(&self.make_indexed_array).is_some() {
                if initial_value.is_some() {
                    var.convert_to_indexed_array_for_reassignment(resolved_dynamic_value.as_ref())?;
                } else {
                    var.convert_to_indexed_array(resolved_dynamic_value.as_ref())?;
                }
            }

            self.apply_attributes_before_update(var)?;

            if let Some(initial_value) = initial_value {
                // We append for `name+=value`, or if the declaration included
                // an explicit index.
                Self::assign_value(var, initial_value, assigned_index, append)?;
            }

            // Validate existing value when -n is being added to a variable
            // that wasn't given a new value (e.g., `x=x; declare -n x`).
            if let Some(msg) = Self::validate_existing_nameref_value(var) {
                writeln!(context.stderr(), "{}: {msg}", context.command_name)?;
                return Ok(false);
            }

            self.apply_attributes_after_update(var, verb)?;
        } else {
            let unset_type = if change(&self.make_indexed_array).is_some() {
                ShellValueUnsetType::IndexedArray
            } else if change(&self.make_associative_array).is_some() {
                ShellValueUnsetType::AssociativeArray
            } else if name_is_array {
                ShellValueUnsetType::IndexedArray
            } else {
                ShellValueUnsetType::Untyped
            };

            let mut var = ShellVariable::new(ShellValue::Unset(unset_type));

            self.apply_attributes_before_update(&mut var)?;

            if let Some(initial_value) = initial_value {
                Self::assign_value(&mut var, initial_value, assigned_index, append)?;
            }

            // Validate nameref target name after assignment.
            if let Some(msg) = Self::validate_existing_nameref_value(&var) {
                writeln!(context.stderr(), "{}: {msg}", context.command_name)?;
                return Ok(false);
            }

            if context.shell.options().export_variables_on_modification && !var.value().is_array() {
                var.export();
            }

            self.apply_attributes_after_update(&mut var, verb)?;

            let scope = if create_var_local {
                EnvironmentScope::Local
            } else {
                EnvironmentScope::Global
            };

            // N.B. We intentionally use `add()` (no nameref resolution) here. When
            // `declare` creates a brand new variable (e.g., `declare -n ref=target`),
            // we're defining the nameref itself, not writing through an existing one.
            // In functions, `declare`/`local` always creates a new local variable in
            // the current scope, even if a nameref with the same name exists in an
            // outer scope — this matches bash behavior.
            context.shell.env_mut().add(name, var, scope)?;
        }

        Ok(true)
    }

    /// Returns whether a declaration creates a local variable, rather than a global one.
    fn creates_local(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        verb: DeclareVerb,
    ) -> bool {
        matches!(verb, DeclareVerb::Local)
            || (matches!(verb, DeclareVerb::Declare)
                && context.shell.in_function()
                && !self.create_global)
    }

    /// Returns where a declaration looks for the variable it names.
    const fn lookup(create_var_local: bool) -> EnvironmentLookup {
        if create_var_local {
            EnvironmentLookup::OnlyInCurrent(EnvironmentScope::Local)
        } else {
            EnvironmentLookup::Anywhere
        }
    }

    /// Reads the assignments bash finds in a declaration's operands beyond those the
    /// parser does.
    ///
    /// An operand that expanded into an assignment, such as `"x=$v"` or `'a=(${v})'`,
    /// isn't one to the parser, as its word starts with a quote. It's read here: its
    /// subscript, if any, is expanded once, and its value is taken as it is.
    ///
    /// A parenthesized value given to an array (one this declaration makes, or one
    /// already there), whether the parser saw the assignment or not, is then read as
    /// the array's elements, which are expanded as those of any array assignment.
    async fn lift_assignment(
        &self,
        context: &mut brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        declaration: &brush_core::CommandArg,
        verb: DeclareVerb,
    ) -> Result<Lifted, brush_core::Error> {
        // TODO(declare): bash refuses `+a` and `+A` on an array; until that's done, their
        // operands are left as they are, which fails the ones that would assign to one.
        if self.function_names_only
            || self.function_names_or_defs_only
            || change(&self.make_indexed_array) == Some(false)
            || change(&self.make_associative_array) == Some(false)
        {
            return Ok(Lifted::Unchanged);
        }

        let options = context.shell.parser_options();
        let mut assignment = match declaration {
            brush_core::CommandArg::String(s) if s.contains('=') => {
                let Ok(mut assignment) = brush_parser::word::parse_scalar_assignment(s, &options)
                else {
                    return Ok(Lifted::Unchanged);
                };
                if let ast::AssignmentName::ArrayElementName(_, index) = &mut assignment.name {
                    *index = context
                        .shell
                        .basic_expand_string(&context.params, &*index)
                        .await?;
                }
                assignment
            }
            brush_core::CommandArg::Assignment(assignment)
                if self.assigns_array_literal(context, assignment, verb) =>
            {
                assignment.clone()
            }
            _ => return Ok(Lifted::Unchanged),
        };

        let literal = match &assignment.value {
            ast::AssignmentValue::Scalar(literal)
                if self.assigns_array_literal(context, &assignment, verb) =>
            {
                literal
            }
            _ => return Ok(Lifted::Assignment(assignment)),
        };

        // As in bash, a compound value given to an element is the whole array's: the
        // subscript is dropped.
        if let ast::AssignmentName::ArrayElementName(name, _) = &assignment.name {
            assignment.name = ast::AssignmentName::VariableName(name.clone());
        }

        let Some(elements) =
            brush_parser::word::parse_compound_assignment_value(&literal.value, &options)
        else {
            writeln!(
                context.stderr(),
                "{}: `{}': syntax error in array assignment",
                context.command_name,
                literal.value
            )?;
            assignment.value = ast::AssignmentValue::Array(vec![]);
            return Ok(Lifted::Malformed(assignment));
        };

        assignment.value = context
            .shell
            .expand_assignment_value(&context.params, &ast::AssignmentValue::Array(elements))
            .await?;
        Ok(Lifted::Assignment(assignment))
    }

    /// Returns whether an assignment gives a parenthesized scalar value to an array,
    /// which bash reads as the array's elements: to a whole array this declaration
    /// makes or that's already there, or to an element of one this declaration makes.
    fn assigns_array_literal(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        assignment: &ast::Assignment,
        verb: DeclareVerb,
    ) -> bool {
        let ast::AssignmentValue::Scalar(value) = &assignment.value else {
            return false;
        };
        value.value.starts_with('(')
            && value.value.ends_with(')')
            && match &assignment.name {
                ast::AssignmentName::VariableName(name) => self.assigns_array(context, name, verb),
                ast::AssignmentName::ArrayElementName(..) => self.makes_array(),
            }
    }

    /// Returns whether this declaration makes its variables arrays.
    fn makes_array(&self) -> bool {
        change(&self.make_indexed_array) == Some(true)
            || change(&self.make_associative_array) == Some(true)
    }

    /// Returns whether a declaration assigns to an array: one it makes, or one that's
    /// already there.
    fn assigns_array(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        name: &str,
        verb: DeclareVerb,
    ) -> bool {
        self.makes_array()
            || context
                .shell
                .env()
                .lookup(name)
                .in_scope(Self::lookup(self.creates_local(context, verb)))
                .get_direct()
                .is_some_and(|(_, var)| var.value().is_array())
    }

    /// Assigns a declaration's value: an element's at its index, as `a[i]=v` and
    /// `a[i]+=v` do, and any other to the whole variable, `+=` appending to it.
    fn assign_value(
        var: &mut ShellVariable,
        value: ShellValueLiteral,
        assigned_index: Option<String>,
        append: bool,
    ) -> Result<(), brush_core::Error> {
        match (assigned_index, value) {
            (Some(index), ShellValueLiteral::Scalar(value)) => {
                var.assign_at_index(index, value, append)
            }
            (index, value) => var.assign(value, append || index.is_some()),
        }
    }

    #[expect(clippy::type_complexity)]
    #[expect(clippy::too_many_lines)]
    fn declaration_to_name_and_value(
        declaration: &brush_core::CommandArg,
    ) -> Result<
        (
            String,
            Option<String>,
            Option<ShellValueLiteral>,
            bool,
            bool,
        ),
        brush_core::Error,
    > {
        let name;
        let assigned_index;
        let initial_value;
        let name_is_array;
        let mut append = false;

        match declaration {
            brush_core::CommandArg::String(s) => {
                #[allow(
                    clippy::unwrap_in_result,
                    clippy::unwrap_used,
                    reason = "regex is valid and should not fail"
                )]
                static NAME_INDEX_AND_VALUE_RE: LazyLock<fancy_regex::Regex> =
                    LazyLock::new(|| {
                        fancy_regex::Regex::new(r"^(.*?)\[([^\]]*)\](\+)?=(.*)$").unwrap()
                    });

                if let Some(captures) = NAME_INDEX_AND_VALUE_RE.captures(s)? {
                    name = captures
                        .get(1)
                        .ok_or_else(|| {
                            brush_core::ErrorKind::InternalError("declaration parse error".into())
                        })?
                        .as_str()
                        .to_owned();

                    assigned_index = captures.get(2).map(|m| m.as_str().to_owned());
                    append = captures.get(3).is_some();
                    initial_value = captures
                        .get(4)
                        .map(|m| ShellValueLiteral::Scalar(m.as_str().to_owned()));
                    name_is_array = true;
                } else if let Some((n, v)) = s.split_once('=') {
                    // `name+=value` appends (runtime split of an expanded word).
                    let n = match n.strip_suffix('+') {
                        Some(stripped) => {
                            append = true;
                            stripped
                        }
                        None => n,
                    };
                    name = n.to_owned();
                    assigned_index = None;
                    initial_value = Some(ShellValueLiteral::Scalar(v.to_owned()));
                    name_is_array = false;
                } else {
                    #[allow(
                        clippy::unwrap_in_result,
                        clippy::unwrap_used,
                        reason = "regex is valid and should not fail"
                    )]
                    static ARRAY_AND_INDEX_RE: LazyLock<fancy_regex::Regex> =
                        LazyLock::new(|| fancy_regex::Regex::new(r"^(.*?)\[(.*?)\]$").unwrap());

                    if let Some(captures) = ARRAY_AND_INDEX_RE.captures(s)? {
                        name = captures
                            .get(1)
                            .ok_or_else(|| {
                                brush_core::ErrorKind::InternalError(
                                    "declaration parse error".into(),
                                )
                            })?
                            .as_str()
                            .to_owned();

                        assigned_index = captures.get(2).map(|m| m.as_str().to_owned());
                        name_is_array = true;
                    } else {
                        name = s.clone();
                        assigned_index = None;
                        name_is_array = false;
                    }
                    initial_value = None;
                }
            }
            brush_core::CommandArg::Assignment(assignment) => {
                append = assignment.append;
                match &assignment.name {
                    ast::AssignmentName::VariableName(var_name) => {
                        name = var_name.to_owned();
                        assigned_index = None;
                    }
                    ast::AssignmentName::ArrayElementName(var_name, index) => {
                        if matches!(assignment.value, ast::AssignmentValue::Array(_)) {
                            return Err(ErrorKind::AssigningListToArrayMember.into());
                        }

                        name = var_name.to_owned();
                        assigned_index = Some(index.to_owned());
                    }
                }

                match &assignment.value {
                    ast::AssignmentValue::Scalar(s) => {
                        // An element's value stays a scalar, to be assigned at its index.
                        initial_value = Some(ShellValueLiteral::Scalar(s.value.clone()));
                        name_is_array = assigned_index.is_some();
                    }
                    ast::AssignmentValue::Array(a) => {
                        initial_value = Some(ShellValueLiteral::Array(ArrayLiteral::new(
                            a.iter()
                                .map(|(i, v)| {
                                    (i.as_ref().map(|w| w.value.clone()), v.value.clone())
                                })
                                .collect(),
                        )));
                        name_is_array = true;
                    }
                }
            }
        }

        Ok((name, assigned_index, initial_value, name_is_array, append))
    }

    /// Validates a nameref target string: must be a legal variable name (optionally
    /// with a `[subscript]` suffix) and must not be a self-reference.
    /// Returns `Some(error_message)` on failure, `None` if valid.
    fn validate_nameref_creation_target(var_name: &str, target: &str) -> Option<String> {
        if target.is_empty() {
            return None;
        }
        if target == var_name {
            return Some(format!(
                "{var_name}: nameref variable self references not allowed"
            ));
        }
        if !env::valid_nameref_target_name(target) {
            return Some(format!(
                "`{target}': invalid variable name for name reference"
            ));
        }
        None
    }

    /// If `var` is a nameref with a non-empty target, validates the target name.
    ///
    /// Unlike [`validate_nameref_creation_target`], this does NOT reject self-references.
    /// Bash allows implicit self-references (e.g., `x=x; declare -n x`) — only
    /// explicit ones at creation time (`declare -n x=x`) are rejected.
    fn validate_existing_nameref_value(var: &ShellVariable) -> Option<String> {
        if !var.is_treated_as_nameref() {
            return None;
        }
        if let ShellValue::String(target) = var.value() {
            if target.is_empty() {
                return None;
            }
            // Only validate the name format, not self-reference.
            if !env::valid_nameref_target_name(target) {
                return Some(format!(
                    "`{target}': invalid variable name for name reference"
                ));
            }
        }
        None
    }

    /// Determines whether this declaration will effectively create a nameref,
    /// accounting for flag conflicts (`-na`, `-nA`) that suppress `-n`.
    ///
    /// The `-ni` combination does NOT suppress `-n` — bash applies both
    /// attributes when no initial value is provided. (With an initial value,
    /// the declaration is rejected early in `process_declaration`.)
    fn will_be_nameref(&self) -> bool {
        matches!(change(&self.make_nameref), Some(true))
            && !(change(&self.make_indexed_array).is_some()
                || change(&self.make_associative_array).is_some())
    }

    /// Resolves namerefs for attribute-changing declarations.
    ///
    /// In bash, when an attribute-changing `declare`/`readonly` command targets an
    /// existing nameref without explicitly setting/unsetting the `-n` flag, the
    /// operation resolves through the nameref and applies to the target variable.
    ///
    /// # Examples of when resolution applies
    ///
    /// ```bash
    /// declare -n ref=target
    /// declare -a ref       # applies -a to "target", not to "ref"
    /// readonly ref          # makes "target" readonly, not "ref"
    /// ```
    ///
    /// # When resolution does NOT apply
    ///
    /// ```bash
    /// f() { declare -n ref=target; }  # creates new local "ref", doesn't resolve
    /// declare -n ref=x                # explicitly setting -n: defines "ref" itself
    /// declare +n ref                  # explicitly unsetting -n: modifies "ref" itself
    /// ```
    ///
    /// The condition: resolve when `-n` is neither being set nor unset (`.is_none()`)
    /// AND the variable already exists in the lookup scope (not creating a new local).
    fn resolve_nameref_for_declaration(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        name: String,
        lookup: EnvironmentLookup,
        create_var_local: bool,
    ) -> Result<(String, EnvironmentLookup), brush_core::Error> {
        // When `-n` is explicitly being set or unset, we're operating on the
        // nameref variable itself — don't resolve through it.
        let explicitly_modifying_nameref_attr = change(&self.make_nameref).is_some();

        // When `declare`/`local` would create a brand-new local variable
        // (rather than modifying an existing one), we don't resolve either —
        // e.g., `f() { declare -n ref=target; }` defines `ref`, not `target`.
        //
        // The existence check below does a scope-stack walk filtered by `lookup`
        // policy. This is short-circuited when `create_var_local` is false (the
        // common case at the global scope), so the walk only runs inside function
        // bodies where each declare/local is a single statement, not a hot loop.
        // The scope stack is small (typically 1-3 entries per nested function),
        // so the per-call cost is negligible.
        let creating_new_local = create_var_local
            && context
                .shell
                .env()
                .lookup(name.as_str().direct())
                .in_scope(lookup)
                .get_direct()
                .is_none();

        let should_resolve = !explicitly_modifying_nameref_attr && !creating_new_local;
        if should_resolve {
            let resolved = context.shell.env().resolve_nameref(name.as_str())?;
            if resolved.name() != name.as_str() {
                // For subscripted targets (e.g., ref→arr[2]), resolve to the
                // base variable name. `declare -x ref` applies the export to
                // the base array `arr`, not to element `arr[2]`. (The
                // standalone `export`/`readonly` commands handle subscripted
                // nameref rejection themselves.)
                return Ok((resolved.into_name(), EnvironmentLookup::Anywhere));
            }
        }
        Ok((name, lookup))
    }

    /// Validates the initial value (if any) as a nameref target.
    /// Returns `Some(error_message)` if the target is invalid.
    fn validate_initial_nameref_target(
        var_name: &str,
        initial_value: Option<&ShellValueLiteral>,
    ) -> Option<String> {
        if let Some(ShellValueLiteral::Scalar(target)) = initial_value {
            Self::validate_nameref_creation_target(var_name, target.as_str())
        } else {
            None
        }
    }

    fn display_matching_env_declarations(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        verb: DeclareVerb,
        output: &mut Vec<u8>,
    ) -> Result<(), brush_core::Error> {
        #[expect(clippy::type_complexity)]
        let mut filters: Vec<Box<dyn Fn((&String, &ShellVariable)) -> bool>> =
            vec![Box::new(|(_, v)| v.is_enumerable())];

        if matches!(verb, DeclareVerb::Readonly) {
            filters.push(Box::new(|(_, v)| v.is_readonly()));
        }

        // Add filters depending on attribute flags. Only `-X` filters; `+X` doesn't.
        let given = |options: &[bool]| options.contains(&true);
        if given(&self.make_indexed_array) {
            filters.push(Box::new(|(_, v)| {
                matches!(v.value(), ShellValue::IndexedArray(_))
            }));
        }
        if given(&self.make_associative_array) {
            filters.push(Box::new(|(_, v)| {
                matches!(v.value(), ShellValue::AssociativeArray(_))
            }));
        }
        if given(&self.make_integer) {
            filters.push(Box::new(|(_, v)| v.is_treated_as_integer()));
        }
        if given(&self.capitalize_value_on_assignment) {
            filters.push(Box::new(|(_, v)| {
                matches!(
                    v.get_update_transform(),
                    ShellVariableUpdateTransform::Capitalize
                )
            }));
        }
        if given(&self.lowercase_value_on_assignment) {
            filters.push(Box::new(|(_, v)| {
                matches!(
                    v.get_update_transform(),
                    ShellVariableUpdateTransform::Lowercase
                )
            }));
        }
        if given(&self.make_nameref) {
            filters.push(Box::new(|(_, v)| v.is_treated_as_nameref()));
        }
        if given(&self.make_readonly) {
            filters.push(Box::new(|(_, v)| v.is_readonly()));
        }
        if given(&self.make_traced) {
            filters.push(Box::new(|(_, v)| v.is_trace_enabled()));
        }
        if given(&self.uppercase_value_on_assignment) {
            filters.push(Box::new(|(_, v)| {
                matches!(
                    v.get_update_transform(),
                    ShellVariableUpdateTransform::Uppercase
                )
            }));
        }
        if given(&self.make_exported) {
            filters.push(Box::new(|(_, v)| v.is_exported()));
        }

        let iter_policy = if matches!(verb, DeclareVerb::Local) {
            EnvironmentLookup::OnlyInCurrent(EnvironmentScope::Local)
        } else {
            EnvironmentLookup::Anywhere
        };

        for (name, variable) in context
            .shell
            .env()
            .iter_using_policy(iter_policy)
            .filter(|pair| filters.iter().all(|f| f(*pair)))
            .sorted_by_key(|v| v.0)
        {
            if self.print {
                let mut cs = variable.attribute_flags(context.shell);
                if cs.is_empty() {
                    cs.push('-');
                }

                let separator_str = if matches!(variable.value(), ShellValue::Unset(_)) {
                    ""
                } else {
                    "="
                };

                writeln!(
                    output,
                    "declare -{cs} {name}{separator_str}{}",
                    variable
                        .value()
                        .format(variables::FormatStyle::DeclarePrint, context.shell)?
                )?;
            } else {
                writeln!(
                    output,
                    "{name}={}",
                    variable
                        .value()
                        .format(variables::FormatStyle::Basic, context.shell)?
                )?;
            }
        }

        Ok(())
    }

    fn display_matching_functions(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        output: &mut Vec<u8>,
    ) -> Result<(), brush_core::Error> {
        for (name, registration) in context.shell.funcs().iter().sorted_by_key(|v| v.0) {
            if self.function_names_only {
                writeln!(output, "declare -f {name}")?;
            } else {
                writeln!(output, "{}", registration.definition())?;
            }
        }

        Ok(())
    }

    #[expect(clippy::unnecessary_wraps)]
    fn apply_attributes_before_update(
        &self,
        var: &mut ShellVariable,
    ) -> Result<(), brush_core::Error> {
        // In bash, -n (nameref) conflicts with certain value-type attributes
        // when an initial value is provided. That case is rejected early in
        // process_declaration. When both flags are set without a value (e.g.,
        // `declare -ni var`), bash applies both attributes. For arrays:
        //   -na: -n dropped, -a kept (creates indexed array, not a nameref)
        //   -nA: -n dropped, -A kept (creates assoc array, not a nameref)
        // The -l, -u, -c flags do NOT conflict with -n.
        let requesting_nameref = matches!(change(&self.make_nameref), Some(true));
        let nameref_array_conflict = requesting_nameref
            && (change(&self.make_indexed_array).is_some()
                || change(&self.make_associative_array).is_some());
        let suppress_nameref = nameref_array_conflict;

        if let Some(value) = change(&self.make_integer) {
            if value {
                var.treat_as_integer();
            } else {
                var.unset_treat_as_integer();
            }
        }
        if let Some(value) = change(&self.capitalize_value_on_assignment) {
            if value {
                var.set_update_transform(ShellVariableUpdateTransform::Capitalize);
            } else if matches!(
                var.get_update_transform(),
                ShellVariableUpdateTransform::Capitalize
            ) {
                var.set_update_transform(ShellVariableUpdateTransform::None);
            }
        }
        if let Some(value) = change(&self.lowercase_value_on_assignment) {
            if value {
                var.set_update_transform(ShellVariableUpdateTransform::Lowercase);
            } else if matches!(
                var.get_update_transform(),
                ShellVariableUpdateTransform::Lowercase
            ) {
                var.set_update_transform(ShellVariableUpdateTransform::None);
            }
        }
        if !suppress_nameref {
            if let Some(value) = change(&self.make_nameref) {
                if value {
                    var.treat_as_nameref();
                } else {
                    var.unset_treat_as_nameref();
                }
            }
        }
        if let Some(value) = change(&self.make_traced) {
            if value {
                var.enable_trace();
            } else {
                var.disable_trace();
            }
        }
        if let Some(value) = change(&self.uppercase_value_on_assignment) {
            if value {
                var.set_update_transform(ShellVariableUpdateTransform::Uppercase);
            } else if matches!(
                var.get_update_transform(),
                ShellVariableUpdateTransform::Uppercase
            ) {
                var.set_update_transform(ShellVariableUpdateTransform::None);
            }
        }
        if let Some(value) = change(&self.make_exported) {
            if value {
                var.export();
            } else {
                var.unexport();
            }
        }

        Ok(())
    }

    fn apply_attributes_after_update(
        &self,
        var: &mut ShellVariable,
        verb: DeclareVerb,
    ) -> Result<(), brush_core::Error> {
        if matches!(verb, DeclareVerb::Readonly) {
            var.set_readonly();
        } else if let Some(value) = change(&self.make_readonly) {
            if value {
                var.set_readonly();
            } else {
                var.unset_readonly()?;
            }
        }

        Ok(())
    }
}
