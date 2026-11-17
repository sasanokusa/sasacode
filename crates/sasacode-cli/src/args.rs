//! Node's `util.parseArgs` in strict mode with positionals allowed, as Bun runs it: the same
//! tokens, the same values and the same error texts (the CLI's flags are an interface, ADR-003).

use indexmap::IndexMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    String,
    Boolean,
}

#[derive(Clone, Copy, Debug)]
pub struct OptionSpec {
    pub name: &'static str,
    pub kind: Kind,
    pub short: Option<char>,
    pub default: Option<&'static str>,
}

pub const fn string(name: &'static str, short: Option<char>) -> OptionSpec {
    OptionSpec { name, kind: Kind::String, short, default: None }
}

pub const fn boolean(name: &'static str, short: Option<char>) -> OptionSpec {
    OptionSpec { name, kind: Kind::Boolean, short, default: None }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Bool(bool),
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub values: IndexMap<String, Value>,
    pub positionals: Vec<String>,
}

impl Parsed {
    pub fn string(&self, name: &str) -> Option<&str> {
        match self.values.get(name) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }
    pub fn flag(&self, name: &str) -> bool {
        matches!(self.values.get(name), Some(Value::Bool(true)))
    }
}

struct Token {
    name: String,
    raw_name: String,
    value: Option<String>,
    inline_value: bool,
}

fn find_long_for_short(short: char, options: &[OptionSpec]) -> String {
    options
        .iter()
        .find(|o| o.short == Some(short))
        .map(|o| o.name.to_string())
        .unwrap_or_else(|| short.to_string())
}

fn kind_of(name: &str, options: &[OptionSpec]) -> Option<Kind> {
    options.iter().find(|o| o.name == name).map(|o| o.kind)
}

/// A JS string's code units, so `arg[1]` and `arg.length` mean what they meant there.
fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn is_lone_short(arg: &[u16]) -> bool {
    arg.len() == 2 && arg[0] == b'-' as u16 && arg[1] != b'-' as u16
}

fn char_at(arg: &str, unit: usize) -> char {
    let u = units(arg);
    char::decode_utf16(u[unit..].iter().copied())
        .next()
        .and_then(Result::ok)
        .unwrap_or('\u{FFFD}')
}

fn slice_from(arg: &str, unit: usize) -> String {
    String::from_utf16_lossy(&units(arg)[unit..])
}

fn starts_short(arg: &[u16]) -> bool {
    arg.len() > 2 && arg[0] == b'-' as u16 && arg[1] != b'-' as u16
}

fn is_option_like(value: &str) -> bool {
    value.encode_utf16().count() > 1 && value.starts_with('-')
}

fn unknown(raw: &str) -> String {
    format!(
        "Unknown option '{raw}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- {}",
        serde_json::to_string(raw).unwrap_or_default()
    )
}

/// `parseArgs({ args, options, allowPositionals: true })`; strict, so problems are errors.
pub fn parse_args(args: &[String], options: &[OptionSpec]) -> Result<Parsed, String> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut rest: std::collections::VecDeque<String> = args.iter().cloned().collect();
    while let Some(arg) = rest.pop_front() {
        let u = units(&arg);
        if arg == "--" {
            positionals.extend(rest.drain(..));
            break;
        }
        if is_lone_short(&u) {
            let short = char_at(&arg, 1);
            let name = find_long_for_short(short, options);
            let (value, inline_value) = match (kind_of(&name, options), rest.front()) {
                (Some(Kind::String), Some(_)) => (rest.pop_front(), false),
                _ => (None, false),
            };
            tokens.push(Token { name, raw_name: arg, value, inline_value });
            continue;
        }
        if starts_short(&u) {
            let first = char_at(&arg, 1);
            if kind_of(&find_long_for_short(first, options), options) != Some(Kind::String) {
                // A short option group: -fXzy is -f -X -z -y, and a string option inside it takes the rest.
                let mut expanded = Vec::new();
                let mut index = 1;
                while index < u.len() {
                    let short = char_at(&arg, index);
                    let width = short.len_utf16();
                    let long = find_long_for_short(short, options);
                    if kind_of(&long, options) != Some(Kind::String) || index + width >= u.len() {
                        expanded.push(format!("-{short}"));
                    } else {
                        expanded.push(format!("-{}", slice_from(&arg, index)));
                        break;
                    }
                    index += width;
                }
                for e in expanded.into_iter().rev() {
                    rest.push_front(e);
                }
                continue;
            }
            // -fVALUE
            let name = find_long_for_short(first, options);
            tokens.push(Token {
                name,
                raw_name: format!("-{first}"),
                value: Some(slice_from(&arg, 1 + first.len_utf16())),
                inline_value: true,
            });
            continue;
        }
        if u.len() > 2 && arg.starts_with("--") {
            match arg[2..].find('=') {
                // --name=value (an '=' from the third unit on)
                Some(i) if i > 0 => {
                    let name = arg[2..2 + i].to_string();
                    let value = arg[2 + i + 1..].to_string();
                    tokens.push(Token { raw_name: format!("--{name}"), name, value: Some(value), inline_value: true });
                }
                _ => {
                    let name = arg[2..].to_string();
                    let (value, inline_value) = match (kind_of(&name, options), rest.front()) {
                        (Some(Kind::String), Some(_)) => (rest.pop_front(), false),
                        _ => (None, false),
                    };
                    tokens.push(Token { name, raw_name: arg, value, inline_value });
                }
            }
            continue;
        }
        positionals.push(arg);
    }

    let mut values: IndexMap<String, Value> = IndexMap::new();
    for token in tokens {
        let Some(spec) = options.iter().find(|o| o.name == token.name) else {
            return Err(unknown(&token.raw_name));
        };
        let short_and_long = match spec.short {
            Some(s) => format!("-{s}, --{}", spec.name),
            None => format!("--{}", spec.name),
        };
        match spec.kind {
            Kind::String if token.value.is_none() => {
                return Err(format!("Option '{short_and_long} <value>' argument missing"));
            }
            Kind::Boolean if token.value.is_some() => {
                return Err(format!("Option '{short_and_long}' does not take an argument"));
            }
            _ => {}
        }
        if let Some(v) = &token.value
            && !token.inline_value
            && is_option_like(v)
        {
            let example = if token.raw_name.starts_with("--") {
                format!("'{}=-XYZ'", token.raw_name)
            } else {
                format!("'--{}=-XYZ' or '{}-XYZ'", token.name, token.raw_name)
            };
            return Err(format!(
                "Option '{raw}' argument is ambiguous.\nDid you forget to specify the option argument for '{raw}'?\nTo specify an option argument starting with a dash use {example}.",
                raw = token.raw_name
            ));
        }
        let value = match token.value {
            Some(v) => Value::String(v),
            None => Value::Bool(true),
        };
        values.insert(token.name, value);
    }
    for o in options {
        if let Some(d) = o.default
            && !values.contains_key(o.name)
        {
            values.insert(o.name.to_string(), Value::String(d.to_string()));
        }
    }
    Ok(Parsed { values, positionals })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: &[OptionSpec] = &[
        string("print", Some('p')),
        OptionSpec { name: "output", kind: Kind::String, short: None, default: Some("text") },
        string("model", Some('m')),
        boolean("continue", Some('c')),
        boolean("no-session", None),
        boolean("help", Some('h')),
    ];

    fn run(args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match parse_args(&args, OPTIONS) {
            Ok(p) => {
                let mut v = serde_json::Map::new();
                for (k, val) in &p.values {
                    v.insert(
                        k.clone(),
                        match val {
                            Value::String(s) => serde_json::Value::from(s.clone()),
                            Value::Bool(b) => serde_json::Value::from(*b),
                        },
                    );
                }
                format!("=> {} {}", serde_json::Value::Object(v), serde_json::to_string(&p.positionals).unwrap())
            }
            Err(e) => format!("!! {e}"),
        }
    }

    /// Each expectation is what `bun` printed for the same call (values in the order Bun stored
    /// them, the default last).
    #[test]
    fn matches_bun() {
        let cases: &[(&[&str], &str)] = &[
            (&["--foo"], "!! Unknown option '--foo'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--foo\""),
            (&["-x"], "!! Unknown option '-x'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"-x\""),
            (&["-p"], "!! Option '-p, --print <value>' argument missing"),
            (&["--model", "-x"], "!! Option '--model' argument is ambiguous.\nDid you forget to specify the option argument for '--model'?\nTo specify an option argument starting with a dash use '--model=-XYZ'."),
            (&["-m", "-x"], "!! Option '-m' argument is ambiguous.\nDid you forget to specify the option argument for '-m'?\nTo specify an option argument starting with a dash use '--model=-XYZ' or '-m-XYZ'."),
            (&["--continue=1"], "!! Option '-c, --continue' does not take an argument"),
            (&["-cp", "hi"], r#"=> {"continue":true,"print":"hi","output":"text"} []"#),
            (&["-pc"], r#"=> {"print":"c","output":"text"} []"#),
            (&["-cm", "a/b", "rest"], r#"=> {"continue":true,"model":"a/b","output":"text"} ["rest"]"#),
            (&["--", "-p"], r#"=> {"output":"text"} ["-p"]"#),
            (&["a", "-p", "x", "b"], r#"=> {"print":"x","output":"text"} ["a","b"]"#),
            (&["--print=", "x"], r#"=> {"print":"","output":"text"} ["x"]"#),
            (&["-p", "--"], "!! Option '-p' argument is ambiguous.\nDid you forget to specify the option argument for '-p'?\nTo specify an option argument starting with a dash use '--print=-XYZ' or '-p-XYZ'."),
            (&["--model=-x"], r#"=> {"model":"-x","output":"text"} []"#),
            (&["-m-x"], r#"=> {"model":"-x","output":"text"} []"#),
            (&["-c", "-c"], r#"=> {"continue":true,"output":"text"} []"#),
            (&["--no-session", "--output", "jsonl"], r#"=> {"no-session":true,"output":"jsonl"} []"#),
            (&["-"], r#"=> {"output":"text"} ["-"]"#),
            (&["--print", "a", "--print", "b"], r#"=> {"print":"b","output":"text"} []"#),
            (&["-hc"], r#"=> {"help":true,"continue":true,"output":"text"} []"#),
            (&["--Print", "x"], "!! Unknown option '--Print'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--Print\""),
            (&["-p", "-"], r#"=> {"print":"-","output":"text"} []"#),
            (&["--no-session=1"], "!! Option '--no-session' does not take an argument"),
            (&["--output"], "!! Option '--output <value>' argument missing"),
            (&["-p", "日本語"], r#"=> {"print":"日本語","output":"text"} []"#),
        ];
        for (args, want) in cases {
            assert_eq!(&run(args), want, "{args:?}");
        }
    }
}
