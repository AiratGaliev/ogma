//! Owned syntax projection at the Oxc/frontend boundary.
//!
//! This is a deliberately small transport view for the compiler. It preserves
//! syntax shape, lexical binding identities and source ranges, but contains no
//! Core IR instructions, CFG blocks, SSA values or backend semantics.

use oxc_ast::ast::{self, Program};
use oxc_ast_visit::{Visit, walk};
use oxc_span::GetSpan;
use oxc_syntax::scope::ScopeId;

use crate::{ResolvedFunction, SemanticSnapshot, SourceFile, SourceSpan};

/// Literal value represented in the M1 syntax subset.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxLiteral {
    /// JavaScript null.
    Null,
    /// Boolean literal.
    Boolean(bool),
    /// ECMAScript Number literal.
    Number(f64),
    /// String literal after escape processing.
    String(String),
    /// BigInt decimal payload without the n suffix.
    Bigint(String),
}

/// Binary operator represented in the M1 syntax subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxBinaryOperator {
    /// JavaScript addition with dynamic coercion.
    Add,
    /// Strict equality.
    StrictEqual,
    /// Strict inequality.
    StrictNotEqual,
    /// Less-than.
    LessThan,
    /// Greater-than.
    GreaterThan,
}

/// Unary operator represented in the M1 syntax subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxUnaryOperator {
    /// Logical negation.
    LogicalNot,
    /// JavaScript typeof.
    Typeof,
    /// Unary numeric negation.
    Negate,
    /// Unary numeric plus.
    Positive,
}

/// Prefix or postfix update operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxUpdateOperator {
    /// Increment by one.
    Increment,
    /// Decrement by one.
    Decrement,
}

/// Property key for a member read or write.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxMemberKey {
    /// Static identifier property key.
    Static(String),
    /// Computed property key.
    Computed(Box<SyntaxExpression>),
}

/// Assignment target represented by the M1 syntax subset.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxAssignmentTarget {
    /// Resolved or unresolved identifier target.
    Identifier {
        /// Identifier spelling.
        name: String,
        /// Resolved lexical binding identity.
        binding: Option<String>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Property or element assignment target.
    Member {
        /// Base object expression.
        object: Box<SyntaxExpression>,
        /// Static or computed key.
        key: SyntaxMemberKey,
        /// Original source range.
        span: SourceSpan,
    },
    /// Syntax target outside the M1 subset.
    Unsupported {
        /// Stable syntax category.
        syntax: String,
        /// Original source range.
        span: SourceSpan,
    },
}

/// Expression projection retaining Oxc-resolved binding identities.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxExpression {
    /// Literal.
    Literal {
        /// Literal payload.
        value: SyntaxLiteral,
        /// Original source range.
        span: SourceSpan,
    },
    /// Identifier reference.
    Identifier {
        /// Identifier spelling.
        name: String,
        /// Resolved lexical binding, or None for a free/global name.
        binding: Option<String>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Binary expression.
    Binary {
        /// Normalized operator.
        operator: SyntaxBinaryOperator,
        /// Left expression.
        left: Box<SyntaxExpression>,
        /// Right expression.
        right: Box<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Unary expression.
    Unary {
        /// Normalized operator.
        operator: SyntaxUnaryOperator,
        /// Argument.
        argument: Box<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Function call.
    Call {
        /// Callee.
        callee: Box<SyntaxExpression>,
        /// Arguments in source order.
        arguments: Vec<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Property or element access.
    Member {
        /// Base object.
        object: Box<SyntaxExpression>,
        /// Property key.
        key: SyntaxMemberKey,
        /// Original source range.
        span: SourceSpan,
    },
    /// Simple assignment.
    Assign {
        /// Assignment target.
        target: SyntaxAssignmentTarget,
        /// Assigned value.
        value: Box<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Prefix/postfix update.
    Update {
        /// Update target.
        target: SyntaxAssignmentTarget,
        /// Increment or decrement.
        operator: SyntaxUpdateOperator,
        /// Prefix or postfix form.
        prefix: bool,
        /// Original source range.
        span: SourceSpan,
    },
    /// Closure construction.
    Function {
        /// Stable nested function identity.
        function: String,
        /// Captured binding identities.
        captures: Vec<String>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Parenthesized expression.
    Parenthesized {
        /// Inner expression.
        expression: Box<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Syntax outside the currently lowered subset.
    Unsupported {
        /// Stable syntax category.
        syntax: String,
        /// Original source range.
        span: SourceSpan,
    },
}

/// Variable declaration kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxVariableKind {
    /// Function-scoped var.
    Var,
    /// Block-scoped let.
    Let,
    /// Immutable const.
    Const,
    /// Resource/other declaration not admitted by the M1 subset.
    Unsupported,
}

/// One simple identifier declarator.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxDeclarator {
    /// Resolved declaration binding identity.
    pub binding: Option<String>,
    /// Declared identifier.
    pub name: String,
    /// Optional initializer expression.
    pub initializer: Option<SyntaxExpression>,
    /// Original declarator range.
    pub span: SourceSpan,
}

/// Initializer of a C-style for loop.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxForInitializer {
    /// Variable declaration initializer.
    Variable {
        /// Declaration kind.
        kind: SyntaxVariableKind,
        /// Declarators in source order.
        declarations: Vec<SyntaxDeclarator>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Expression initializer.
    Expression(SyntaxExpression),
}

/// Statement projection retaining lexical scope identities.
#[derive(Clone, Debug, PartialEq)]
pub enum SyntaxStatement {
    /// Lexical block.
    Block {
        /// Oxc lexical scope identity, if present.
        scope: Option<String>,
        /// Nested statements in source order.
        body: Vec<SyntaxStatement>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Variable declaration.
    Variable {
        /// Declaration kind.
        kind: SyntaxVariableKind,
        /// Declarators in source order.
        declarations: Vec<SyntaxDeclarator>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Function declaration.
    FunctionDeclaration {
        /// Stable function identity.
        function: String,
        /// Binding in the enclosing lexical environment.
        binding: Option<String>,
        /// Declared name.
        name: String,
        /// Original source range.
        span: SourceSpan,
    },
    /// Expression statement.
    Expression {
        /// Expression value.
        expression: SyntaxExpression,
        /// Original source range.
        span: SourceSpan,
    },
    /// Conditional branch.
    If {
        /// Condition expression.
        test: SyntaxExpression,
        /// Consequent statement.
        consequent: Box<SyntaxStatement>,
        /// Optional alternate statement.
        alternate: Option<Box<SyntaxStatement>>,
        /// Original source range.
        span: SourceSpan,
    },
    /// while loop.
    While {
        /// Loop condition.
        test: SyntaxExpression,
        /// Loop body.
        body: Box<SyntaxStatement>,
        /// Original source range.
        span: SourceSpan,
    },
    /// C-style for loop.
    For {
        /// Loop lexical scope.
        scope: Option<String>,
        /// Optional initializer.
        initializer: Option<SyntaxForInitializer>,
        /// Optional condition.
        test: Option<SyntaxExpression>,
        /// Optional update expression.
        update: Option<SyntaxExpression>,
        /// Loop body.
        body: Box<SyntaxStatement>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Return statement.
    Return {
        /// Optional returned expression.
        value: Option<SyntaxExpression>,
        /// Original source range.
        span: SourceSpan,
    },
    /// Throw statement.
    Throw {
        /// Thrown expression.
        value: SyntaxExpression,
        /// Original source range.
        span: SourceSpan,
    },
    /// Empty statement.
    Empty {
        /// Original source range.
        span: SourceSpan,
    },
    /// Syntax outside the currently lowered subset.
    Unsupported {
        /// Stable syntax category.
        syntax: String,
        /// Original source range.
        span: SourceSpan,
    },
}

/// A function or Program unit that can be lowered independently.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSyntaxFunction {
    /// Stable frontend function identity.
    pub id: String,
    /// Human-readable function name.
    pub name: String,
    /// Function lexical scope identity.
    pub scope: String,
    /// Enclosing Program/function scope identity.
    pub parent_function_scope: Option<String>,
    /// Simple parameter binding identities in source order.
    pub parameters: Vec<String>,
    /// Captured lexical binding identities.
    pub captures: Vec<String>,
    /// Function body in source order.
    pub body: Vec<SyntaxStatement>,
    /// Original function/Program source range.
    pub span: SourceSpan,
    /// Whether this function is an arrow function.
    pub is_arrow: bool,
    /// Whether this function is async.
    pub is_async: bool,
    /// Whether this function is a generator.
    pub is_generator: bool,
    /// Whether a named function expression has a private self-name binding.
    pub has_private_self_name: bool,
}

/// Owned syntax transport. The compiler turns it into canonical Core IR v1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedSyntaxProgram {
    /// Program unit followed by nested function units in source order.
    pub functions: Vec<ResolvedSyntaxFunction>,
}

fn scope_id(scope: ScopeId) -> String {
    format!("scope_{}", scope.index())
}

fn resolved_function<'a>(
    scope: ScopeId,
    semantic: &'a SemanticSnapshot,
) -> Option<&'a ResolvedFunction> {
    let id = scope_id(scope);
    semantic
        .functions
        .iter()
        .find(|function| function.scope == id)
}

fn binding_id(symbol: Option<oxc_syntax::symbol::SymbolId>) -> Option<String> {
    symbol.map(|symbol| format!("binding_{}", symbol.index()))
}

fn resolved_reference(
    identifier: &ast::IdentifierReference<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxExpression {
    let span = source.span(identifier.span.start as usize, identifier.span.end as usize);
    let binding = semantic
        .references
        .iter()
        .find(|reference| {
            reference.span.start_byte == span.start_byte && reference.span.end_byte == span.end_byte
        })
        .and_then(|reference| reference.binding.clone());
    SyntaxExpression::Identifier {
        name: identifier.name.as_str().to_owned(),
        binding,
        span,
    }
}

fn expression(
    node: &ast::Expression<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxExpression {
    use ast::Expression as E;
    let node_span = node.span();
    let span = source.span(node_span.start as usize, node_span.end as usize);
    match node {
        E::NullLiteral(_) => SyntaxExpression::Literal {
            value: SyntaxLiteral::Null,
            span,
        },
        E::BooleanLiteral(literal) => SyntaxExpression::Literal {
            value: SyntaxLiteral::Boolean(literal.value),
            span,
        },
        E::NumericLiteral(literal) => SyntaxExpression::Literal {
            value: SyntaxLiteral::Number(literal.value),
            span,
        },
        E::StringLiteral(literal) => {
            let Some(value) = literal.value.as_str() else {
                return SyntaxExpression::Unsupported {
                    syntax: "string_literal_with_lone_surrogate".to_owned(),
                    span,
                };
            };
            SyntaxExpression::Literal {
                value: SyntaxLiteral::String(value.to_owned()),
                span,
            }
        }
        E::BigIntLiteral(literal) => SyntaxExpression::Literal {
            value: SyntaxLiteral::Bigint(literal.value.to_string()),
            span,
        },
        E::Identifier(identifier) => resolved_reference(identifier, semantic, source),
        E::ParenthesizedExpression(parenthesized) => SyntaxExpression::Parenthesized {
            expression: Box::new(expression(&parenthesized.expression, semantic, source)),
            span,
        },
        E::BinaryExpression(binary) => {
            use oxc_syntax::operator::BinaryOperator as B;
            let (operator, left, right) = match binary.operator {
                B::Addition => (SyntaxBinaryOperator::Add, &binary.left, &binary.right),
                B::StrictEquality => (
                    SyntaxBinaryOperator::StrictEqual,
                    &binary.left,
                    &binary.right,
                ),
                B::StrictInequality => (
                    SyntaxBinaryOperator::StrictNotEqual,
                    &binary.left,
                    &binary.right,
                ),
                B::LessThan => (SyntaxBinaryOperator::LessThan, &binary.left, &binary.right),
                B::GreaterThan => (
                    SyntaxBinaryOperator::GreaterThan,
                    &binary.left,
                    &binary.right,
                ),
                _ => {
                    return SyntaxExpression::Unsupported {
                        syntax: format!("binary_operator:{}", binary.operator.as_str()),
                        span,
                    };
                }
            };
            SyntaxExpression::Binary {
                operator,
                left: Box::new(expression(left, semantic, source)),
                right: Box::new(expression(right, semantic, source)),
                span,
            }
        }
        E::UnaryExpression(unary) => {
            use oxc_syntax::operator::UnaryOperator as U;
            let operator = match unary.operator {
                U::LogicalNot => SyntaxUnaryOperator::LogicalNot,
                U::Typeof => SyntaxUnaryOperator::Typeof,
                U::UnaryNegation => SyntaxUnaryOperator::Negate,
                U::UnaryPlus => SyntaxUnaryOperator::Positive,
                _ => {
                    return SyntaxExpression::Unsupported {
                        syntax: format!("unary_operator:{:?}", unary.operator),
                        span,
                    };
                }
            };
            SyntaxExpression::Unary {
                operator,
                argument: Box::new(expression(&unary.argument, semantic, source)),
                span,
            }
        }
        E::CallExpression(call) if !call.optional => {
            let mut arguments = Vec::with_capacity(call.arguments.len());
            for argument in &call.arguments {
                let Some(value) = argument.as_expression() else {
                    return SyntaxExpression::Unsupported {
                        syntax: "spread_call_argument".to_owned(),
                        span,
                    };
                };
                arguments.push(expression(value, semantic, source));
            }
            SyntaxExpression::Call {
                callee: Box::new(expression(&call.callee, semantic, source)),
                arguments,
                span,
            }
        }
        E::StaticMemberExpression(member) if !member.optional => SyntaxExpression::Member {
            object: Box::new(expression(&member.object, semantic, source)),
            key: SyntaxMemberKey::Static(member.property.name.as_str().to_owned()),
            span,
        },
        E::ComputedMemberExpression(member) if !member.optional => SyntaxExpression::Member {
            object: Box::new(expression(&member.object, semantic, source)),
            key: SyntaxMemberKey::Computed(Box::new(expression(
                &member.expression,
                semantic,
                source,
            ))),
            span,
        },
        E::AssignmentExpression(assignment)
            if matches!(
                assignment.operator,
                oxc_syntax::operator::AssignmentOperator::Assign
            ) =>
        {
            SyntaxExpression::Assign {
                target: assignment_target(&assignment.left, semantic, source),
                value: Box::new(expression(&assignment.right, semantic, source)),
                span,
            }
        }
        E::UpdateExpression(update) => SyntaxExpression::Update {
            target: simple_assignment_target(&update.argument, semantic, source),
            operator: match update.operator {
                oxc_syntax::operator::UpdateOperator::Increment => SyntaxUpdateOperator::Increment,
                oxc_syntax::operator::UpdateOperator::Decrement => SyntaxUpdateOperator::Decrement,
            },
            prefix: update.prefix,
            span,
        },
        E::FunctionExpression(function) => {
            let Some(scope) = function.scope_id.get() else {
                return SyntaxExpression::Unsupported {
                    syntax: "function_expression_without_scope".to_owned(),
                    span,
                };
            };
            let Some(resolved) = resolved_function(scope, semantic) else {
                return SyntaxExpression::Unsupported {
                    syntax: "function_expression_scope_missing".to_owned(),
                    span,
                };
            };
            if function.id.is_some() {
                return SyntaxExpression::Unsupported {
                    syntax: "named_function_expression_self_binding".to_owned(),
                    span,
                };
            }
            SyntaxExpression::Function {
                function: resolved.id.clone(),
                captures: resolved.captures.clone(),
                span,
            }
        }
        E::ArrowFunctionExpression(function) => {
            let Some(scope) = function.scope_id.get() else {
                return SyntaxExpression::Unsupported {
                    syntax: "arrow_function_without_scope".to_owned(),
                    span,
                };
            };
            let Some(resolved) = resolved_function(scope, semantic) else {
                return SyntaxExpression::Unsupported {
                    syntax: "arrow_function_scope_missing".to_owned(),
                    span,
                };
            };
            SyntaxExpression::Function {
                function: resolved.id.clone(),
                captures: resolved.captures.clone(),
                span,
            }
        }
        _ => SyntaxExpression::Unsupported {
            syntax: format!("expression:{:?}", std::mem::discriminant(node)),
            span,
        },
    }
}

fn assignment_target(
    node: &ast::AssignmentTarget<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxAssignmentTarget {
    let Some(simple) = node.as_simple_assignment_target() else {
        return SyntaxAssignmentTarget::Unsupported {
            syntax: "destructuring_assignment_target".to_owned(),
            span: source.span(node.span().start as usize, node.span().end as usize),
        };
    };
    simple_assignment_target(simple, semantic, source)
}

fn simple_assignment_target(
    node: &ast::SimpleAssignmentTarget<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxAssignmentTarget {
    let ast_span = node.span();
    let span = source.span(ast_span.start as usize, ast_span.end as usize);
    match node {
        ast::SimpleAssignmentTarget::AssignmentTargetIdentifier(identifier) => {
            let identifier_span =
                source.span(identifier.span.start as usize, identifier.span.end as usize);
            let binding = semantic
                .references
                .iter()
                .find(|reference| {
                    reference.span.start_byte == identifier_span.start_byte
                        && reference.span.end_byte == identifier_span.end_byte
                })
                .and_then(|reference| reference.binding.clone());
            SyntaxAssignmentTarget::Identifier {
                name: identifier.name.as_str().to_owned(),
                binding,
                span,
            }
        }
        ast::SimpleAssignmentTarget::StaticMemberExpression(member) if !member.optional => {
            SyntaxAssignmentTarget::Member {
                object: Box::new(expression(&member.object, semantic, source)),
                key: SyntaxMemberKey::Static(member.property.name.as_str().to_owned()),
                span,
            }
        }
        ast::SimpleAssignmentTarget::ComputedMemberExpression(member) if !member.optional => {
            SyntaxAssignmentTarget::Member {
                object: Box::new(expression(&member.object, semantic, source)),
                key: SyntaxMemberKey::Computed(Box::new(expression(
                    &member.expression,
                    semantic,
                    source,
                ))),
                span,
            }
        }
        _ => SyntaxAssignmentTarget::Unsupported {
            syntax: "assignment_target".to_owned(),
            span,
        },
    }
}

fn declaration_kind(kind: ast::VariableDeclarationKind) -> SyntaxVariableKind {
    match kind {
        ast::VariableDeclarationKind::Var => SyntaxVariableKind::Var,
        ast::VariableDeclarationKind::Let => SyntaxVariableKind::Let,
        ast::VariableDeclarationKind::Const => SyntaxVariableKind::Const,
        ast::VariableDeclarationKind::Using | ast::VariableDeclarationKind::AwaitUsing => {
            SyntaxVariableKind::Unsupported
        }
    }
}

fn variable_declaration(
    node: &ast::VariableDeclaration<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> (SyntaxVariableKind, Vec<SyntaxDeclarator>, SourceSpan) {
    let ast_span = node.span();
    let span = source.span(ast_span.start as usize, ast_span.end as usize);
    let declarations = node
        .declarations
        .iter()
        .map(|declarator| {
            let decl_span =
                source.span(declarator.span.start as usize, declarator.span.end as usize);
            match &declarator.id {
                ast::BindingPattern::BindingIdentifier(identifier) => SyntaxDeclarator {
                    binding: binding_id(identifier.symbol_id.get()),
                    name: identifier.name.as_str().to_owned(),
                    initializer: declarator
                        .init
                        .as_ref()
                        .map(|init| expression(init, semantic, source)),
                    span: decl_span,
                },
                _ => SyntaxDeclarator {
                    binding: None,
                    name: "<destructuring>".to_owned(),
                    initializer: declarator
                        .init
                        .as_ref()
                        .map(|init| expression(init, semantic, source)),
                    span: decl_span,
                },
            }
        })
        .collect();
    (declaration_kind(node.kind), declarations, span)
}

fn syntax_statement(
    node: &ast::Statement<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxStatement {
    use ast::Statement as S;
    let ast_span = node.span();
    let span = source.span(ast_span.start as usize, ast_span.end as usize);
    match node {
        S::EmptyStatement(_) => SyntaxStatement::Empty { span },
        S::BlockStatement(block) => SyntaxStatement::Block {
            scope: block.scope_id.get().map(scope_id),
            body: block
                .body
                .iter()
                .map(|item| syntax_statement(item, semantic, source))
                .collect(),
            span,
        },
        S::VariableDeclaration(declaration) => {
            let (kind, declarations, span) = variable_declaration(declaration, semantic, source);
            SyntaxStatement::Variable {
                kind,
                declarations,
                span,
            }
        }
        S::FunctionDeclaration(function) => function_declaration(function, semantic, source),
        S::ExpressionStatement(expression_statement) => SyntaxStatement::Expression {
            expression: expression(&expression_statement.expression, semantic, source),
            span,
        },
        S::IfStatement(if_statement) => SyntaxStatement::If {
            test: expression(&if_statement.test, semantic, source),
            consequent: Box::new(syntax_statement(&if_statement.consequent, semantic, source)),
            alternate: if_statement
                .alternate
                .as_ref()
                .map(|alternate| Box::new(syntax_statement(alternate, semantic, source))),
            span,
        },
        S::WhileStatement(loop_statement) => SyntaxStatement::While {
            test: expression(&loop_statement.test, semantic, source),
            body: Box::new(syntax_statement(&loop_statement.body, semantic, source)),
            span,
        },
        S::ForStatement(loop_statement) => {
            let initializer = loop_statement.init.as_ref().map(|init| match init {
                ast::ForStatementInit::VariableDeclaration(declaration) => {
                    let (kind, declarations, span) =
                        variable_declaration(declaration, semantic, source);
                    SyntaxForInitializer::Variable {
                        kind,
                        declarations,
                        span,
                    }
                }
                _ => init
                    .as_expression()
                    .map(|expression_node| {
                        SyntaxForInitializer::Expression(expression(
                            expression_node,
                            semantic,
                            source,
                        ))
                    })
                    .unwrap_or_else(|| {
                        SyntaxForInitializer::Expression(SyntaxExpression::Unsupported {
                            syntax: "for_initializer".to_owned(),
                            span,
                        })
                    }),
            });
            SyntaxStatement::For {
                scope: loop_statement.scope_id.get().map(scope_id),
                initializer,
                test: loop_statement
                    .test
                    .as_ref()
                    .map(|test| expression(test, semantic, source)),
                update: loop_statement
                    .update
                    .as_ref()
                    .map(|update| expression(update, semantic, source)),
                body: Box::new(syntax_statement(&loop_statement.body, semantic, source)),
                span,
            }
        }
        S::ReturnStatement(return_statement) => SyntaxStatement::Return {
            value: return_statement
                .argument
                .as_ref()
                .map(|value| expression(value, semantic, source)),
            span,
        },
        S::ThrowStatement(throw_statement) => SyntaxStatement::Throw {
            value: expression(&throw_statement.argument, semantic, source),
            span,
        },
        _ => SyntaxStatement::Unsupported {
            syntax: format!("statement:{:?}", std::mem::discriminant(node)),
            span,
        },
    }
}

fn function_declaration(
    node: &ast::Function<'_>,
    semantic: &SemanticSnapshot,
    source: &SourceFile,
) -> SyntaxStatement {
    let ast_span = node.span();
    let span = source.span(ast_span.start as usize, ast_span.end as usize);
    let Some(scope) = node.scope_id.get() else {
        return SyntaxStatement::Unsupported {
            syntax: "function_declaration_without_scope".to_owned(),
            span,
        };
    };
    let Some(function) = resolved_function(scope, semantic) else {
        return SyntaxStatement::Unsupported {
            syntax: "function_declaration_scope_missing".to_owned(),
            span,
        };
    };
    let id = node.id.as_ref();
    SyntaxStatement::FunctionDeclaration {
        function: function.id.clone(),
        binding: id.and_then(|identifier| binding_id(identifier.symbol_id.get())),
        name: id
            .map(|identifier| identifier.name.as_str().to_owned())
            .unwrap_or_default(),
        span,
    }
}

struct FunctionCollector<'s> {
    source: &'s SourceFile,
    semantic: &'s SemanticSnapshot,
    functions: Vec<ResolvedSyntaxFunction>,
}

impl<'a> Visit<'a> for FunctionCollector<'_> {
    fn visit_function(&mut self, node: &ast::Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
        let span = self
            .source
            .span(node.span.start as usize, node.span.end as usize);
        if let Some(scope) = node.scope_id.get()
            && let Some(resolved) = resolved_function(scope, self.semantic)
        {
            let body = node
                .body
                .as_ref()
                .map(|body| {
                    body.statements
                        .iter()
                        .map(|statement| syntax_statement(statement, self.semantic, self.source))
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![SyntaxStatement::Unsupported {
                        syntax: "function_without_body".to_owned(),
                        span,
                    }]
                });
            self.functions.push(ResolvedSyntaxFunction {
                id: resolved.id.clone(),
                name: resolved.name.clone(),
                scope: resolved.scope.clone(),
                parent_function_scope: resolved.parent_function_scope.clone(),
                parameters: resolved.parameters.clone(),
                captures: resolved.captures.clone(),
                body,
                span: resolved.span,
                is_arrow: false,
                is_async: resolved.is_async,
                is_generator: resolved.is_generator,
                has_private_self_name: node.r#type == ast::FunctionType::FunctionExpression
                    && node.id.is_some(),
            });
        }
        walk::walk_function(self, node, flags);
    }

    fn visit_arrow_function_expression(&mut self, node: &ast::ArrowFunctionExpression<'a>) {
        if let Some(scope) = node.scope_id.get()
            && let Some(resolved) = resolved_function(scope, self.semantic)
        {
            let body = match &node.body {
                ast::ArrowFunctionBody::FunctionBody(body) => body
                    .statements
                    .iter()
                    .map(|statement| syntax_statement(statement, self.semantic, self.source))
                    .collect(),
                _ => node
                    .body
                    .as_expression()
                    .map(|value| {
                        vec![SyntaxStatement::Return {
                            value: Some(expression(value, self.semantic, self.source)),
                            span: self.source.span(
                                node.body.span().start as usize,
                                node.body.span().end as usize,
                            ),
                        }]
                    })
                    .unwrap_or_else(|| {
                        vec![SyntaxStatement::Unsupported {
                            syntax: "arrow_expression_body".to_owned(),
                            span: self
                                .source
                                .span(node.span.start as usize, node.span.end as usize),
                        }]
                    }),
            };
            self.functions.push(ResolvedSyntaxFunction {
                id: resolved.id.clone(),
                name: resolved.name.clone(),
                scope: resolved.scope.clone(),
                parent_function_scope: resolved.parent_function_scope.clone(),
                parameters: resolved.parameters.clone(),
                captures: resolved.captures.clone(),
                body,
                span: resolved.span,
                is_arrow: true,
                is_async: resolved.is_async,
                is_generator: resolved.is_generator,
                has_private_self_name: false,
            });
        }
        walk::walk_arrow_function_expression(self, node);
    }
}

/// Projects an Oxc AST into the frontend-owned M1 syntax transport.
pub(crate) fn build_syntax_program(
    program: &Program<'_>,
    source: &SourceFile,
    semantic: &SemanticSnapshot,
) -> ResolvedSyntaxProgram {
    let program_function = semantic
        .functions
        .iter()
        .find(|function| function.id == "fn_program");
    let program_span = source.span(0, source.text().len());
    let program_body = program
        .body
        .iter()
        .map(|statement| syntax_statement(statement, semantic, source))
        .collect();
    let mut functions = vec![ResolvedSyntaxFunction {
        id: "fn_program".to_owned(),
        name: "<program>".to_owned(),
        scope: semantic.root_scope.clone(),
        parent_function_scope: None,
        parameters: Vec::new(),
        captures: Vec::new(),
        body: program_body,
        span: program_function
            .map(|function| function.span)
            .unwrap_or(program_span),
        is_arrow: false,
        is_async: false,
        is_generator: false,
        has_private_self_name: false,
    }];
    let mut collector = FunctionCollector {
        source,
        semantic,
        functions: Vec::new(),
    };
    collector.visit_program(program);
    collector.functions.sort_by_key(|function| {
        (
            function.span.start_byte,
            function.span.end_byte,
            function.id.clone(),
        )
    });
    functions.extend(collector.functions);
    ResolvedSyntaxProgram { functions }
}
