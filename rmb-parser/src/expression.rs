use miette::{Error, LabeledSpan};

use crate::Expression;
use rmb_lexer::{
    token::{Kind, Location, Operator, Primitive, Token, Value},
    Lexer, TransposeRef,
};

mod precedences {
    pub const BASE: u8 = 0;
    pub const SUM: u8 = 3;
    pub const MUL: u8 = 4;
    pub const ASSOC: u8 = 5;
    pub const APPLY: u8 = 6;
}

fn get_precedence(operator: Operator) -> u8 {
    match operator {
        Operator::Plus | Operator::Minus => precedences::SUM,
        Operator::Star | Operator::Slash => precedences::MUL,
        Operator::And => precedences::ASSOC,
        Operator::LeftParen | Operator::EqualEqual | Operator::NotEqual => precedences::APPLY,
        _ => precedences::BASE,
    }
}

pub fn parse_expression<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    match lexer.peek().transpose()? {
        Some(token) => match token.kind {
            Kind::Var | Kind::Const => parse_variable(lexer),
            _ => parse_with_precedence(lexer, precedences::BASE),
        },
        None => unreachable!(),
    }
}

pub fn parse_identifier<'parser>(lexer: &mut Lexer<'parser>) -> Result<(Expression<'parser>, &'parser str), Error> {
    let name_and_loc = lexer.next().transpose()?.map(|token| match token.kind {
        Kind::Value(Value::Ident(name)) => (name, token.location),
        _ => ("", token.location),
    });

    match name_and_loc {
        Some(("", location)) => Err(miette::miette! {
            labels = vec![
                LabeledSpan::at(location.start_byte..location.end_byte, "this identifier"),
            ],
            "",
        }
        .with_source_code(lexer.complete_source.to_string())),
        Some((name, location)) => Ok((Expression::Ident { name, location }, name)),
        None => {
            let location = lexer.complete_source.len() - 1..lexer.complete_source.len();
            Err(miette::miette! {
                labels = vec![
                    LabeledSpan::at(location, "at this location"),
                ],
                "unexpected end of file",
            }
            .with_source_code(lexer.complete_source.to_string()))
        }
    }
}

fn parse_expr_block<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let mut expressions = vec![];

    let block_start = lexer.expect(Kind::Op(Operator::LeftBrace))?;

    loop {
        match lexer.peek().transpose()? {
            Some(token) => {
                if let Kind::Op(Operator::RightBrace) = token.kind {
                    break;
                }
            }
            None => break,
        }

        let expr = parse_expression(lexer)?;
        expressions.push(expr);
    }

    let block_end = lexer.expect(Kind::Op(Operator::RightBrace))?;

    let location = block_start.location.start_byte..block_end.location.end_byte;
    Ok(Expression::Block {
        expressions,
        location: location.into(),
    })
}

fn parse_variable<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let keyword = lexer.expect_one_of(&[Kind::Var, Kind::Const])?;
    let mutable = matches!(keyword.kind, Kind::Var);

    let (_, name) = parse_identifier(lexer)?;

    let typ = match lexer.peek().transpose()? {
        Some(token) if matches!(token.kind, Kind::Op(Operator::Colon)) => {
            lexer.next().transpose()?;
            Some(parse_identifier(lexer)?)
        }
        _ => None,
    };

    lexer.expect(Kind::Op(Operator::Equal))?;

    let value = match lexer.peek().transpose()? {
        Some(token) => match token.kind {
            Kind::Op(Operator::LeftBrace) => parse_expr_block(lexer)?,
            _ => parse_expression(lexer)?,
        },
        _ => unreachable!(),
    };

    lexer.expect(Kind::Op(Operator::SemiColon))?;

    let location = Location::new(keyword.location.start_byte, value.location().end_byte);
    Ok(Expression::Var {
        mutable,
        typ: typ.map(|(typ, _)| Box::new(typ)),
        name,
        value: Box::new(value),
        location,
    })
}

fn parse_if_expression<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let keyword = lexer.expect(Kind::If)?;

    let condition = parse_expression(lexer)?;

    let body = parse_expr_block(lexer)?;

    let mut falsy_branches = vec![];

    while let Some(keyword) = lexer.peek().transpose()? {
        if keyword.kind == Kind::Else {
            // consume `else`
            lexer.next().transpose()?;

            if lexer
                .peek()
                .transpose()?
                .map(|t| matches!(t.kind, Kind::If))
                .unwrap_or(false)
            {
                let else_if = parse_if_expression(lexer)?;
                falsy_branches.push(else_if);
            } else {
                let else_block = parse_expr_block(lexer)?;
                falsy_branches.push(else_block);
                break;
            }
        } else {
            break;
        }
    }

    let end = match falsy_branches.last() {
        Some(branch) => branch.location().end_byte,
        None => body.location().end_byte,
    };

    let location = keyword.location.start_byte..end;
    Ok(Expression::If {
        condition: Box::new(condition),
        truthy: Box::new(body),
        falsy: falsy_branches,
        location: location.into(),
    })
}

fn parse_value<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let Some(Token { kind, .. }) = lexer.peek().transpose()? else {
        unreachable!();
    };

    let Kind::Value(value) = kind else {
        unreachable!();
    };

    match value {
        Value::Primitive(_) => parse_primitive(lexer),
        Value::Ident(_) => Ok(parse_identifier(lexer)?.0),
        t => todo!("{t}"),
    }
}

fn parse_operation<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let Some(Token { kind, .. }) = lexer.peek().transpose()? else {
        unreachable!();
    };

    let Kind::Op(op) = kind else {
        unreachable!();
    };

    match op {
        Operator::LeftParen => {
            lexer.next().transpose()?;
            let left = parse_with_precedence(lexer, precedences::BASE)?;
            lexer.expect(Kind::Op(Operator::RightParen))?;
            Ok(left)
        }
        t => todo!("{t:?}"),
    }
}

fn parse_fun_call<'parser>(
    lexer: &mut Lexer<'parser>,
    ident: Expression<'parser>,
) -> Result<Expression<'parser>, Error> {
    lexer.expect(Kind::Op(Operator::LeftParen))?;

    let mut arguments = vec![];

    loop {
        match lexer.peek().transpose()? {
            Some(token) if matches!(token.kind, Kind::Op(Operator::RightParen)) => {
                break;
            }
            Some(token) if matches!(token.kind, Kind::Op(Operator::Comma)) => {
                lexer.next().transpose()?;
                continue;
            }
            None => break,
            _ => (),
        }

        let arg = parse_expression(lexer)?;

        arguments.push(arg);
    }

    let close_paren = lexer.expect(Kind::Op(Operator::RightParen))?;

    let location = ident.location().start_byte..close_paren.location.end_byte;
    let expr = Expression::FunCall {
        ident: Box::new(ident),
        location: location.into(),
        arguments,
    };
    Ok(expr)
}

fn parse_assign<'parser>(lexer: &mut Lexer<'parser>, left: Expression<'parser>) -> Result<Expression<'parser>, Error> {
    lexer.expect(Kind::Op(Operator::Equal))?;

    let value = match lexer.peek().transpose()? {
        Some(token) if matches!(token.kind, Kind::Op(Operator::LeftBrace)) => parse_expr_block(lexer)?,
        Some(_) => parse_expression(lexer)?,
        _ => unreachable!(),
    };

    let closing = lexer.expect(Kind::Op(Operator::SemiColon))?;

    let location = left.location().start_byte..closing.location.end_byte;

    Ok(Expression::Assign {
        ident: Box::new(left),
        location: location.into(),
        value: Box::new(value),
    })
}

fn parse_with_precedence<'parser>(
    lexer: &mut Lexer<'parser>,
    min_precedence: u8,
) -> Result<Expression<'parser>, Error> {
    let mut left = match lexer.peek().transpose()? {
        Some(token) => match &token.kind {
            Kind::Value(_) => parse_value(lexer)?,
            Kind::Op(_) => parse_operation(lexer)?,
            Kind::Return => parse_return_expression(lexer)?,
            Kind::If => parse_if_expression(lexer)?,
            t => todo!("{t:?}"),
        },
        None => todo!(),
    };

    if let Expression::Ident { .. } = left {
        match lexer.peek().transpose()? {
            Some(token) if matches!(token.kind, Kind::Op(Operator::LeftParen)) => return parse_fun_call(lexer, left),
            Some(token) if matches!(token.kind, Kind::Op(Operator::Equal)) => return parse_assign(lexer, left),
            _ => (),
        }
    }

    loop {
        let Some(next) = lexer.peek().transpose()? else {
            return Ok(left);
        };

        let Kind::Op(operator) = next.kind else {
            return Ok(left);
        };

        if !next.kind.is_binary_op() {
            return Ok(left);
        }

        let precedence = get_precedence(operator);

        if precedence <= min_precedence {
            break;
        }

        let Some(_) = lexer.next().transpose()? else {
            unreachable!();
        };

        let right = parse_with_precedence(lexer, precedence)?;

        let location = Location::new(left.location().start_byte, right.location().end_byte);
        left = Expression::BinaryOp {
            lhs: Box::new(left),
            operator,
            rhs: Box::new(right),
            location,
        };
    }

    Ok(left)
}

fn parse_return_expression<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let keyword = lexer.expect(Kind::Return)?;

    let value = parse_expression(lexer)?;

    let ending_semi = lexer.expect(Kind::Op(Operator::SemiColon))?;

    let location = keyword.location.start_byte..ending_semi.location.end_byte;
    Ok(Expression::Return {
        value: Box::new(value),
        location: location.into(),
    })
}

fn parse_primitive<'parser>(lexer: &mut Lexer<'parser>) -> Result<Expression<'parser>, Error> {
    let (primitive, location) = match lexer.next().transpose()? {
        Some(Token {
            kind: Kind::Value(Value::Primitive(primitive)),
            location,
            ..
        }) => (primitive, location),
        _ => unreachable!(),
    };

    match primitive {
        Primitive::Int { value, size } => Ok(Expression::IntLiteral { value, size, location }),
        Primitive::UInt { value, size } => Ok(Expression::UintLiteral { value, size, location }),
        Primitive::Float { value, size } => Ok(Expression::FloatLiteral { value, size, location }),
        Primitive::Bool(value) => Ok(Expression::Bool { value, location }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;

    fn make_sut(source: &str) -> Parser<'_> {
        Parser::new(source)
    }

    #[test]
    fn simple_math() {
        let simple_math_expr = "1 + 2 * 3 - 4";

        let mut parser = make_sut(simple_math_expr);

        let math_expr_ast = match parse_expression(&mut parser.lexer) {
            Ok(expr) => expr,
            Err(e) => panic!("{e:?}"),
        };

        insta::assert_debug_snapshot!(math_expr_ast);
    }

    #[test]
    fn mutable_variable() {
        let variables = "var hello = 1 + 2 * 3;";
        let mut parser = make_sut(variables);

        let variables_ast = match parse_expression(&mut parser.lexer) {
            Ok(expr) => expr,
            Err(e) => panic!("{e:?}"),
        };

        insta::assert_debug_snapshot!(variables_ast);
    }

    #[test]
    fn immutable_constant() {
        let variables = "const hello = 1 + 2 * 3;";
        let mut parser = make_sut(variables);

        let variables_ast = match parse_expression(&mut parser.lexer) {
            Ok(expr) => expr,
            Err(e) => panic!("{e:?}"),
        };

        insta::assert_debug_snapshot!(variables_ast);
    }

    #[test]
    fn if_statement() {
        let source = r#"
            if something == another_thing && 1 != 2 {
                const this_is_a_var = 10 + 3;
            } else if something == 10 {
                const this_is_another = 10 + 10;
            } else {
                const omg = 1 + 1;
            }
        "#;
        let mut parser = make_sut(source);

        let if_ast = match parse_expression(&mut parser.lexer) {
            Ok(expr) => expr,
            Err(e) => panic!("{e:?}"),
        };

        insta::assert_debug_snapshot!(if_ast);
    }

    #[test]
    fn if_as_variable_value() {
        let source = r#"
            const my_var = if something == another_thing {
                return 10 + 10;
            } else {
                return 20 + 20;
            };
        "#;

        let mut parser = make_sut(source);
        let let_if_ast = match parse_expression(&mut parser.lexer) {
            Ok(expr) => expr,
            Err(e) => panic!("{e:?}"),
        };

        insta::assert_debug_snapshot!(let_if_ast);
    }
}
