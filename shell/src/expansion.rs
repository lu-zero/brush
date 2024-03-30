use parser::ast;
use uzers::os::unix::UserExt;

use crate::arithmetic::Evaluatable;
use crate::error;
use crate::patterns;
use crate::prompt;
use crate::shell::Shell;
use crate::variables::ShellValue;

pub(crate) async fn basic_expand_word(
    shell: &mut Shell,
    word: &ast::Word,
) -> Result<String, error::Error> {
    basic_expand_word_str(shell, word.flatten().as_str()).await
}

pub(crate) async fn basic_expand_word_str(
    shell: &mut Shell,
    s: &str,
) -> Result<String, error::Error> {
    let mut expander = WordExpander::new(shell);
    expander.basic_expand(s).await
}

pub(crate) async fn full_expand_and_split_word(
    shell: &mut Shell,
    word: &ast::Word,
) -> Result<Vec<String>, error::Error> {
    let mut expander = WordExpander::new(shell);
    expander
        .full_expand_with_splitting(word.flatten().as_str())
        .await
}

#[derive(Clone)]
enum ExpandedWordPiece {
    Unsplittable(String),
    Splittable(String),
    Separator,
}

impl ExpandedWordPiece {
    fn as_str(&self) -> &str {
        match self {
            ExpandedWordPiece::Unsplittable(s) => s.as_str(),
            ExpandedWordPiece::Splittable(s) => s.as_str(),
            ExpandedWordPiece::Separator => "",
        }
    }

    fn unwrap(self) -> String {
        match self {
            ExpandedWordPiece::Unsplittable(s) => s,
            ExpandedWordPiece::Splittable(s) => s,
            ExpandedWordPiece::Separator => String::new(),
        }
    }
}

type WordField = Vec<ExpandedWordPiece>;

#[derive(Debug)]
struct ParameterExpansionResult {
    pub expansion: ParameterExpansionValue,
    pub parameter_state: ParameterState,
}

#[derive(Debug)]
enum ParameterExpansionValue {
    String(String),
    Array(Vec<String>),
}

impl ParameterExpansionResult {
    fn new(parameter_state: ParameterState) -> Self {
        Self {
            expansion: ParameterExpansionValue::String(String::new()),
            parameter_state,
        }
    }
}

impl From<String> for ParameterExpansionResult {
    fn from(s: String) -> Self {
        let parameter_state = if s.is_empty() {
            ParameterState::DefinedEmptyString
        } else {
            ParameterState::NonZeroLength
        };

        Self {
            expansion: ParameterExpansionValue::String(s),
            parameter_state,
        }
    }
}

impl From<&ShellValue> for ParameterExpansionResult {
    fn from(value: &ShellValue) -> Self {
        match value {
            ShellValue::Unset(_) => Self::new(ParameterState::DeclaredButUnset),
            ShellValue::Random | ShellValue::String(_) => {
                ParameterExpansionResult::from(String::from(value))
            }
            ShellValue::AssociativeArray(values) => {
                let just_values = values.values().map(|v| v.to_owned()).collect();
                let parameter_state = if values.is_empty() {
                    ParameterState::DefinedEmptyString
                } else {
                    ParameterState::NonZeroLength
                };
                Self {
                    expansion: ParameterExpansionValue::Array(just_values),
                    parameter_state,
                }
            }
            ShellValue::IndexedArray(values) => {
                let just_values = values.values().map(|v| v.to_owned()).collect();
                let parameter_state = if values.is_empty() {
                    ParameterState::DefinedEmptyString
                } else {
                    ParameterState::NonZeroLength
                };
                Self {
                    expansion: ParameterExpansionValue::Array(just_values),
                    parameter_state,
                }
            }
        }
    }
}

impl From<ParameterExpansionValue> for String {
    fn from(value: ParameterExpansionValue) -> Self {
        match value {
            ParameterExpansionValue::String(s) => s,
            ParameterExpansionValue::Array(values) => values.join(" "),
        }
    }
}

#[derive(Debug)]
enum ParameterState {
    NonZeroLength,
    DefinedEmptyString,
    DeclaredButUnset,
    Undefined,
}

struct WordExpander<'a> {
    shell: &'a mut Shell,
}

impl<'a> WordExpander<'a> {
    pub fn new(shell: &'a mut Shell) -> Self {
        Self { shell }
    }

    /// Apply tilde-expansion, parameter expansion, command substitution, and arithmetic expansion.
    pub async fn basic_expand(&mut self, word: &str) -> Result<String, error::Error> {
        let expanded_pieces = self.basic_expand_into_pieces(word).await?;
        let flattened = expanded_pieces.into_iter().map(|p| p.unwrap()).collect();
        Ok(flattened)
    }

    /// Apply tilde-expansion, parameter expansion, command substitution, and arithmetic expansion;
    /// yield pieces that could be further processed.
    async fn basic_expand_into_pieces(
        &mut self,
        word: &str,
    ) -> Result<Vec<ExpandedWordPiece>, error::Error> {
        //
        // Expand: tildes, parameters, command substitutions, arithmetic.
        //
        let pieces = parser::parse_word_for_expansion(word).map_err(error::Error::Unknown)?;

        let mut expanded_pieces = vec![];
        for piece in pieces {
            let mut next_expanded_pieces = self.expand_word_piece(&piece).await?;
            expanded_pieces.append(&mut next_expanded_pieces);
        }

        Ok(coalesce_expanded_pieces(expanded_pieces))
    }

    /// Apply tilde-expansion, parameter expansion, command substitution, and arithmetic expansion;
    /// then perform field splitting and pathname expansion.
    pub async fn full_expand_with_splitting(
        &mut self,
        word: &str,
    ) -> Result<Vec<String>, error::Error> {
        // Perform basic expansion first.
        let expanded_pieces = self.basic_expand_into_pieces(word).await?;

        // Then split.
        let fields = self.split_fields(expanded_pieces);

        // Now expand pathnames if necessary. This also unquotes as a side effect.
        let result = fields
            .into_iter()
            .map(|field| {
                if self.shell.options.disable_filename_globbing {
                    self.unquote_field_as_vec(field)
                } else {
                    Ok(self.expand_pathnames_in_field(field))
                }
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flat_map(|v| v.into_iter())
            .collect();

        Ok(result)
    }

    fn split_fields(&self, pieces: Vec<ExpandedWordPiece>) -> Vec<WordField> {
        let ifs = self.shell.get_ifs();

        let mut fields: Vec<WordField> = vec![];
        let mut current_field: WordField = vec![];

        // Go through all the already-expanded pieces in this word.
        for piece in pieces {
            match piece {
                ExpandedWordPiece::Unsplittable(_) => current_field.push(piece),
                ExpandedWordPiece::Separator => {
                    if !current_field.is_empty() {
                        fields.push(current_field);
                        current_field = vec![];
                    }
                }
                ExpandedWordPiece::Splittable(_) => {
                    for c in piece.as_str().chars() {
                        if ifs.contains(c) {
                            if !current_field.is_empty() {
                                fields.push(current_field);
                                current_field = vec![];
                            }
                        } else {
                            match current_field.last_mut() {
                                Some(ExpandedWordPiece::Splittable(last)) => last.push(c),
                                Some(ExpandedWordPiece::Unsplittable(_)) | None => {
                                    current_field
                                        .push(ExpandedWordPiece::Splittable(c.to_string()));
                                }
                                _ => unreachable!(),
                            }
                        }
                    }
                }
            }
        }

        if !current_field.is_empty() {
            fields.push(current_field);
        }

        fields
    }

    async fn basic_expand_opt_word_str(
        &mut self,
        word: &Option<String>,
    ) -> Result<String, error::Error> {
        if let Some(word) = word {
            self.basic_expand(word).await
        } else {
            Ok(String::new())
        }
    }

    fn expand_pathnames_in_field(&self, field: WordField) -> Vec<String> {
        // Expand only items marked splittable.
        let expansion_candidates = field.into_iter().map(|piece| match piece {
            ExpandedWordPiece::Unsplittable(s) => vec![s],
            ExpandedWordPiece::Splittable(s) => self.expand_pathnames_in_string(s),
            ExpandedWordPiece::Separator => vec![],
        });

        // Now generate the cartesian product of all the expansions.
        itertools::Itertools::multi_cartesian_product(expansion_candidates)
            .map(|v| v.join(""))
            .collect()
    }

    #[allow(clippy::unused_self)]
    fn expand_pathnames_in_string(&self, pattern: String) -> Vec<String> {
        match patterns::pattern_expand(pattern.as_str(), self.shell.working_dir.as_path()) {
            Ok(expanded) if !expanded.is_empty() => expanded
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            _ => vec![pattern],
        }
    }

    #[allow(clippy::unused_self)]
    #[allow(clippy::unnecessary_wraps)]
    fn unquote_field_as_vec(&self, field: WordField) -> Result<Vec<String>, error::Error> {
        Ok(vec![self.unquote_field(field)?])
    }

    #[allow(clippy::unused_self)]
    #[allow(clippy::unnecessary_wraps)]
    fn unquote_field(&self, field: WordField) -> Result<String, error::Error> {
        Ok(field.into_iter().map(|piece| piece.unwrap()).collect())
    }

    #[async_recursion::async_recursion]
    async fn expand_word_piece(
        &mut self,
        word_piece: &parser::word::WordPiece,
    ) -> Result<Vec<ExpandedWordPiece>, error::Error> {
        let expansion = match word_piece {
            parser::word::WordPiece::Text(t) => vec![ExpandedWordPiece::Splittable(t.clone())],
            parser::word::WordPiece::SingleQuotedText(t) => {
                vec![ExpandedWordPiece::Unsplittable(t.clone())]
            }
            parser::word::WordPiece::DoubleQuotedSequence(pieces) => {
                let mut results = vec![];
                for piece in pieces {
                    // Expand the piece, and concatenate its raw string contents.
                    let inner_expanded_pieces = self.expand_word_piece(piece).await?;
                    for (i, expanded_piece) in inner_expanded_pieces.into_iter().enumerate() {
                        if matches!(expanded_piece, ExpandedWordPiece::Separator) {
                            results.push(ExpandedWordPiece::Separator);
                        } else if i == 0 {
                            let next_str = expanded_piece.as_str();
                            match results.last_mut() {
                                Some(ExpandedWordPiece::Unsplittable(s)) => s.push_str(next_str),
                                None => results
                                    .push(ExpandedWordPiece::Unsplittable(next_str.to_owned())),
                                Some(_) => unreachable!(),
                            }
                        } else {
                            let next_str = expanded_piece.as_str();
                            results.push(ExpandedWordPiece::Unsplittable(next_str.to_owned()));
                        }
                    }
                }

                results
            }
            parser::word::WordPiece::TildePrefix(prefix) => {
                vec![ExpandedWordPiece::Splittable(
                    self.expand_tilde_expression(prefix)?,
                )]
            }
            parser::word::WordPiece::ParameterExpansion(p) => match p {
                parser::word::ParameterExpr::Parameter {
                    parameter:
                        parser::word::Parameter::Special(
                            parser::word::SpecialParameter::AllPositionalParameters {
                                concatenate: false,
                            },
                        ),
                } => {
                    let result = self
                        .shell
                        .positional_parameters
                        .iter()
                        .map(|p| ExpandedWordPiece::Splittable(p.to_owned()));

                    itertools::Itertools::intersperse(result, ExpandedWordPiece::Separator)
                        .collect()
                }
                parser::word::ParameterExpr::Parameter {
                    parameter:
                        parser::word::Parameter::NamedWithAllIndices {
                            name,
                            concatenate: false,
                        },
                } => match self.shell.env.get(name) {
                    Some(var) => {
                        let result = var
                            .value()
                            .get_all_elements()?
                            .into_iter()
                            .map(ExpandedWordPiece::Splittable);

                        itertools::Itertools::intersperse(result, ExpandedWordPiece::Separator)
                            .collect()
                    }
                    None => vec![],
                },
                _ => {
                    vec![ExpandedWordPiece::Splittable(
                        self.expand_parameter_expr(p).await?,
                    )]
                }
            },
            parser::word::WordPiece::CommandSubstitution(s) => {
                let exec_result = self.shell.run_string(s.as_str(), true).await?;
                let exec_output = exec_result.output;

                if exec_output.is_none() {
                    log::error!("error: no output captured from command substitution");
                }

                let exec_output = exec_output.unwrap_or_else(String::new);

                // We trim trailing newlines, per spec.
                let exec_output = exec_output.trim_end_matches('\n');

                vec![ExpandedWordPiece::Splittable(exec_output.to_owned())]
            }
            parser::word::WordPiece::EscapeSequence(s) => {
                vec![ExpandedWordPiece::Unsplittable(
                    s.strip_prefix('\\').unwrap().to_owned(),
                )]
            }
            parser::word::WordPiece::ArithmeticExpression(e) => {
                vec![ExpandedWordPiece::Splittable(
                    self.expand_arithmetic_expr(e).await?,
                )]
            }
        };

        Ok(expansion)
    }

    fn expand_tilde_expression(&self, prefix: &str) -> Result<String, error::Error> {
        if !prefix.is_empty() {
            return error::unimp("expansion: complex tilde expression");
        }

        if let Some(home) = self.shell.env.get("HOME") {
            return Ok(String::from(home.value()));
        } else {
            // HOME isn't set, so let's query passwd et al. to figure out the current
            // user's home directory.
            if let Some(username) = uzers::get_current_username() {
                if let Some(user_info) = uzers::get_user_by_name(&username) {
                    return Ok(user_info.home_dir().to_string_lossy().to_string());
                }
            }
        }

        // If we still can't figure it out, error out.
        Err(error::Error::TildeWithoutValidHome)
    }

    #[allow(clippy::too_many_lines)]
    async fn expand_parameter_expr(
        &mut self,
        expr: &parser::word::ParameterExpr,
    ) -> Result<String, error::Error> {
        #[allow(clippy::cast_possible_truncation)]
        match expr {
            parser::word::ParameterExpr::Parameter { parameter } => {
                Ok(self.expand_parameter(parameter)?.expansion.into())
            }
            parser::word::ParameterExpr::UseDefaultValues {
                parameter,
                test_type,
                default_value,
            } => {
                let expanded_parameter = self.expand_parameter(parameter)?;
                let default_value = default_value.as_ref().map_or_else(|| "", |v| v.as_str());

                match (test_type, expanded_parameter.parameter_state) {
                    (_, ParameterState::NonZeroLength)
                    | (
                        parser::word::ParameterTestType::Unset,
                        ParameterState::DefinedEmptyString,
                    ) => Ok(expanded_parameter.expansion.into()),
                    _ => Ok(self.basic_expand(default_value).await?),
                }
            }
            parser::word::ParameterExpr::AssignDefaultValues {
                parameter: _,
                test_type: _,
                default_value: _,
            } => error::unimp("expansion: assign default values expressions"),
            parser::word::ParameterExpr::IndicateErrorIfNullOrUnset {
                parameter: _,
                test_type: _,
                error_message: _,
            } => error::unimp("expansion: indicate error if null or unset expressions"),
            parser::word::ParameterExpr::UseAlternativeValue {
                parameter,
                test_type,
                alternative_value,
            } => {
                let expanded_parameter = self.expand_parameter(parameter)?;
                let alternative_value = alternative_value
                    .as_ref()
                    .map_or_else(|| "", |v| v.as_str());

                match (test_type, expanded_parameter.parameter_state) {
                    (_, ParameterState::NonZeroLength)
                    | (
                        parser::word::ParameterTestType::Unset,
                        ParameterState::DefinedEmptyString,
                    ) => Ok(self.basic_expand(alternative_value).await?),
                    _ => Ok(String::new()),
                }
            }
            parser::word::ParameterExpr::ParameterLength { parameter } => {
                let len = match self.expand_parameter(parameter)?.expansion {
                    ParameterExpansionValue::String(s) => s.len(),
                    ParameterExpansionValue::Array(values) => values.len(),
                };

                Ok(len.to_string())
            }
            parser::word::ParameterExpr::RemoveSmallestSuffixPattern { parameter, pattern } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                let expanded_pattern = self.basic_expand_opt_word_str(pattern).await?;
                let result = patterns::remove_smallest_matching_suffix(
                    expanded_parameter.as_str(),
                    expanded_pattern.as_str(),
                )?;
                Ok(result.to_owned())
            }
            parser::word::ParameterExpr::RemoveLargestSuffixPattern { parameter, pattern } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                let expanded_pattern = self.basic_expand_opt_word_str(pattern).await?;
                let result = patterns::remove_largest_matching_suffix(
                    expanded_parameter.as_str(),
                    expanded_pattern.as_str(),
                )?;

                Ok(result.to_owned())
            }
            parser::word::ParameterExpr::RemoveSmallestPrefixPattern { parameter, pattern } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                let expanded_pattern = self.basic_expand_opt_word_str(pattern).await?;
                let result = patterns::remove_smallest_matching_prefix(
                    expanded_parameter.as_str(),
                    expanded_pattern.as_str(),
                )?;

                Ok(result.to_owned())
            }
            parser::word::ParameterExpr::RemoveLargestPrefixPattern { parameter, pattern } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                let expanded_pattern = self.basic_expand_opt_word_str(pattern).await?;
                let result = patterns::remove_largest_matching_prefix(
                    expanded_parameter.as_str(),
                    expanded_pattern.as_str(),
                )?;

                Ok(result.to_owned())
            }
            parser::word::ParameterExpr::Substring {
                parameter,
                offset,
                length,
            } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();

                let expanded_offset = offset.eval(self.shell).await?;
                let expanded_offset = usize::try_from(expanded_offset)
                    .map_err(|e| error::Error::Unknown(e.into()))?;

                if expanded_offset >= expanded_parameter.len() {
                    return Ok(String::new());
                }

                let result = if let Some(length) = length {
                    let mut expanded_length = length.eval(self.shell).await?;
                    if expanded_length < 0 {
                        let param_length: i64 = i64::try_from(expanded_parameter.len())
                            .map_err(|e| error::Error::Unknown(e.into()))?;
                        expanded_length += param_length;
                    }

                    let expanded_length = std::cmp::min(
                        usize::try_from(expanded_length)
                            .map_err(|e| error::Error::Unknown(e.into()))?,
                        expanded_parameter.len() - expanded_offset,
                    );

                    &expanded_parameter[expanded_offset..(expanded_offset + expanded_length)]
                } else {
                    &expanded_parameter[expanded_offset..]
                };

                Ok(result.to_owned())
            }
            parser::word::ParameterExpr::Transform { parameter, op } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                match op {
                    parser::word::ParameterTransformOp::PromptExpand => {
                        let result =
                            prompt::expand_prompt(self.shell, expanded_parameter.as_str())?;
                        Ok(result)
                    }
                    parser::word::ParameterTransformOp::CapitalizeInitial => {
                        Ok(to_initial_capitals(expanded_parameter.as_str()))
                    }
                    parser::word::ParameterTransformOp::ExpandEscapeSequences => {
                        error::unimp("parameter transformation: ExpandEscapeSequences")
                    }
                    parser::word::ParameterTransformOp::PossiblyQuoteWithArraysExpanded {
                        separate_words: _,
                    } => error::unimp("parameter transformation: PossiblyQuoteWithArraysExpanded"),
                    parser::word::ParameterTransformOp::Quoted => {
                        error::unimp("parameter transformation: Quoted")
                    }
                    parser::word::ParameterTransformOp::ToAssignmentLogic => {
                        error::unimp("parameter transformation: ToAssignmentLogic")
                    }
                    parser::word::ParameterTransformOp::ToAttributeFlags => {
                        error::unimp("parameter transformation: ToAttributeFlags")
                    }
                    parser::word::ParameterTransformOp::ToLowerCase => {
                        Ok(expanded_parameter.to_lowercase())
                    }
                    parser::word::ParameterTransformOp::ToUpperCase => {
                        Ok(expanded_parameter.to_uppercase())
                    }
                }
            }
            parser::word::ParameterExpr::UppercaseFirstChar {
                parameter: _parameter,
                pattern: _pattern,
            } => error::unimp("expansion: uppercase first char"),
            parser::word::ParameterExpr::UppercasePattern {
                parameter: _parameter,
                pattern: _pattern,
            } => error::unimp("expansion: uppercase pattern"),
            parser::word::ParameterExpr::LowercaseFirstChar {
                parameter,
                pattern: _pattern,
            } => {
                let expanded_parameter: String = self.expand_parameter(parameter)?.expansion.into();
                if let Some(first_char) = expanded_parameter.chars().next() {
                    let mut result = String::new();
                    result.push(first_char.to_lowercase().next().unwrap());
                    result.push_str(expanded_parameter.get(1..).unwrap());
                    Ok(result)
                } else {
                    Ok(expanded_parameter)
                }
            }
            parser::word::ParameterExpr::LowercasePattern {
                parameter: _parameter,
                pattern: _pattern,
            } => error::unimp("expansion: lowercase pattern"),
            parser::word::ParameterExpr::ReplaceSubstring {
                parameter: _parameter,
                pattern: _patter,
                replacement: _replacement,
                match_kind: _match_kind,
            } => error::unimp("expansion: replace substring"),
            parser::word::ParameterExpr::VariableNames {
                prefix: _prefix,
                concatenate: _concatenate,
            } => error::unimp("expansion: variable names"),
        }
    }

    fn expand_parameter(
        &mut self,
        parameter: &parser::word::Parameter,
    ) -> Result<ParameterExpansionResult, error::Error> {
        match parameter {
            parser::word::Parameter::Positional(p) => {
                if *p == 0 {
                    return Err(anyhow::anyhow!("unexpected positional parameter").into());
                }

                if let Some(parameter) = self.shell.positional_parameters.get((p - 1) as usize) {
                    Ok(ParameterExpansionResult::from(parameter.to_owned()))
                } else {
                    Ok(ParameterExpansionResult::new(ParameterState::Undefined))
                }
            }
            parser::word::Parameter::Special(s) => self.expand_special_parameter(s),
            parser::word::Parameter::Named(n) => {
                if let Some(var) = self.shell.env.get(n) {
                    if matches!(var.value(), ShellValue::Unset(_)) {
                        Ok(ParameterExpansionResult::new(
                            ParameterState::DeclaredButUnset,
                        ))
                    } else {
                        Ok(ParameterExpansionResult::from(String::from(var.value())))
                    }
                } else {
                    Ok(ParameterExpansionResult::new(ParameterState::Undefined))
                }
            }
            parser::word::Parameter::NamedWithIndex { name, index } => {
                if let Some(var) = self.shell.env.get(name) {
                    if matches!(var.value(), ShellValue::Unset(_)) {
                        Ok(ParameterExpansionResult::new(
                            ParameterState::DeclaredButUnset,
                        ))
                    } else {
                        if let Some(value) = var.value().get_at(index.as_str())? {
                            Ok(ParameterExpansionResult::from(value))
                        } else {
                            Ok(ParameterExpansionResult::new(ParameterState::Undefined))
                        }
                    }
                } else {
                    Ok(ParameterExpansionResult::new(ParameterState::Undefined))
                }
            }
            parser::word::Parameter::NamedWithAllIndices {
                name,
                concatenate: _concatenate,
            } => {
                if let Some(var) = self.shell.env.get(name) {
                    if matches!(var.value(), ShellValue::Unset(_)) {
                        Ok(ParameterExpansionResult::new(
                            ParameterState::DeclaredButUnset,
                        ))
                    } else {
                        Ok(ParameterExpansionResult::from(var.value().get_all()?))
                    }
                } else {
                    Ok(ParameterExpansionResult::new(ParameterState::Undefined))
                }
            }
        }
    }

    fn expand_special_parameter(
        &mut self,
        parameter: &parser::word::SpecialParameter,
    ) -> Result<ParameterExpansionResult, error::Error> {
        let expansion = match parameter {
            parser::word::SpecialParameter::AllPositionalParameters { concatenate } => {
                if *concatenate {
                    let separator = self.shell.get_ifs().chars().next().unwrap_or(' ');
                    self.shell
                        .positional_parameters
                        .join(separator.to_string().as_str())
                } else {
                    // TODO: implement concatenate policy
                    self.shell.positional_parameters.join(" ")
                }
            }
            parser::word::SpecialParameter::PositionalParameterCount => {
                self.shell.positional_parameters.len().to_string()
            }
            parser::word::SpecialParameter::LastExitStatus => {
                self.shell.last_exit_status.to_string()
            }
            parser::word::SpecialParameter::CurrentOptionFlags => self.shell.current_option_flags(),
            parser::word::SpecialParameter::ProcessId => std::process::id().to_string(),
            parser::word::SpecialParameter::LastBackgroundProcessId => {
                return error::unimp("expansion: last background process id");
            }
            parser::word::SpecialParameter::ShellName => self
                .shell
                .shell_name
                .as_ref()
                .map_or_else(String::new, |name| name.clone()),
        };

        Ok(ParameterExpansionResult::from(expansion))
    }

    async fn expand_arithmetic_expr(
        &mut self,
        expr: &parser::ast::ArithmeticExpr,
    ) -> Result<String, error::Error> {
        let value = expr.eval(self.shell).await?;
        Ok(value.to_string())
    }
}

fn coalesce_expanded_pieces(pieces: Vec<ExpandedWordPiece>) -> Vec<ExpandedWordPiece> {
    pieces.into_iter().fold(Vec::new(), |mut acc, piece| {
        match piece {
            ExpandedWordPiece::Unsplittable(s) => {
                if let Some(ExpandedWordPiece::Unsplittable(last)) = acc.last_mut() {
                    last.push_str(s.as_str());
                } else {
                    acc.push(ExpandedWordPiece::Unsplittable(s));
                }
            }
            ExpandedWordPiece::Splittable(s) => {
                if let Some(ExpandedWordPiece::Splittable(last)) = acc.last_mut() {
                    last.push_str(s.as_str());
                } else {
                    acc.push(ExpandedWordPiece::Splittable(s));
                }
            }
            ExpandedWordPiece::Separator => {
                acc.push(ExpandedWordPiece::Separator);
            }
        }
        acc
    })
}

fn to_initial_capitals(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c.is_whitespace() {
            capitalize_next = true;
            result.push(c);
        } else if capitalize_next {
            result.push_str(c.to_uppercase().to_string().as_str());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}
