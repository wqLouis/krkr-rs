//! The command line: the command families and the option parser.

use crate::error::ToolError;

/// The tool's usage text.
const USAGE: &str = "\
krkr-devtools — the krkr-rs developer tools (Linux only)

Usage:
  krkr-devtools <family> <command> [options]

XP3 archive tools:
  xp3 info <archive.xp3>
  xp3 list <archive.xp3> [--filter SUBSTR]
  xp3 cat <archive.xp3> <name> [--lines A:B]
  xp3 extract <archive.xp3> <name> -o <path>
  xp3 extract-all <archive.xp3> -o <dir> [--filter SUBSTR]

TJS bytecode tools:
  tjsb info <file.tjsb>
  tjsb strings <file.tjsb> [--min-len N]
  tjsb disasm <file.tjsb> [--object N] [--grep TEXT]

TJS compiler (built with the `tjs-compile` feature):
  tjs compile <in.tjs> -o <out.tjsb>

Every command prints its result on stdout. A failure prints a message on
stderr and exits with a non-zero status: 2 for a command-line failure, 1 for
every other failure.
";

/// One named option a command accepts.
pub struct OptionSpec {
    /// The key the command reads the option's value under.
    pub key: &'static str,
    /// Every spelling the option answers to, the dashes included.
    pub spellings: &'static [&'static str],
    /// Whether the option takes a value.
    pub takes_value: bool,
}

/// What a command accepts on the command line.
pub struct CommandSpec {
    /// The smallest number of positional values the command accepts.
    pub min_positionals: usize,
    /// The largest number of positional values the command accepts.
    pub max_positionals: usize,
    /// The options the command accepts.
    pub options: &'static [OptionSpec],
}

/// The parsed command line of one command.
pub struct Invocation {
    positionals: Vec<String>,
    values: Vec<(String, String)>,
    flags: Vec<String>,
}

impl Invocation {
    /// The positional value at `index`, when the command line has one.
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }

    /// The value of the option named `key`, when the command line has one.
    pub fn value(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Whether the flag named `key` was given.
    pub fn has_flag(&self, key: &str) -> bool {
        self.flags.iter().any(|name| name == key)
    }
}

/// The positional value at `index`, or a usage failure when it is missing.
pub fn positional(invocation: &Invocation, index: usize) -> Result<&str, ToolError> {
    invocation
        .positional(index)
        .ok_or_else(|| ToolError::Usage(format!("missing positional value {index}")))
}

/// Parse one command's arguments.
///
/// A `--` argument ends the option list: every argument after it is a
/// positional value. An unknown option, a repeated option, an option without
/// its value and a wrong number of positional values are all usage failures.
pub fn parse(argv: &[String], spec: &CommandSpec) -> Result<Invocation, ToolError> {
    let mut positionals: Vec<String> = Vec::new();
    let mut values: Vec<(String, String)> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut only_positionals = false;
    let mut index = 0usize;
    while index < argv.len() {
        let argument = argv[index].clone();
        index += 1;
        if only_positionals {
            positionals.push(argument);
            continue;
        }
        if argument == "--" {
            only_positionals = true;
            continue;
        }
        let is_option = argument.len() > 1 && argument.starts_with('-');
        if !is_option {
            positionals.push(argument);
            continue;
        }
        let (spelling, inline) = match argument.split_once('=') {
            Some((spelling, value)) => (spelling.to_string(), Some(value.to_string())),
            None => (argument.clone(), None),
        };
        let Some(option) = spec
            .options
            .iter()
            .find(|option| option.spellings.iter().any(|known| *known == spelling))
        else {
            return Err(ToolError::Usage(format!("unknown option `{spelling}`")));
        };
        if option.takes_value {
            let value = match inline {
                Some(value) => value,
                None => {
                    let value = argv.get(index).cloned().ok_or_else(|| {
                        ToolError::Usage(format!("option `{spelling}` needs a value"))
                    })?;
                    index += 1;
                    value
                }
            };
            if values.iter().any(|(name, _)| name == option.key) {
                return Err(ToolError::Usage(format!(
                    "option `{spelling}` was given twice"
                )));
            }
            values.push((option.key.to_string(), value));
        } else {
            if inline.is_some() {
                return Err(ToolError::Usage(format!(
                    "option `{spelling}` does not take a value"
                )));
            }
            if flags.iter().any(|name| name == option.key) {
                return Err(ToolError::Usage(format!(
                    "option `{spelling}` was given twice"
                )));
            }
            flags.push(option.key.to_string());
        }
    }
    if positionals.len() < spec.min_positionals || positionals.len() > spec.max_positionals {
        return Err(ToolError::Usage(format!(
            "expected {} to {} positional values, found {}",
            spec.min_positionals,
            spec.max_positionals,
            positionals.len()
        )));
    }
    Ok(Invocation {
        positionals,
        values,
        flags,
    })
}

/// Run the command line.
pub fn run(argv: &[String]) -> Result<(), ToolError> {
    let Some(family) = argv.first() else {
        print!("{USAGE}");
        return Err(ToolError::Usage("a command family is required".to_string()));
    };
    if family == "--help" || family == "-h" || family == "help" {
        print!("{USAGE}");
        return Ok(());
    }
    let rest = &argv[1..];
    match family.as_str() {
        #[cfg(feature = "xp3")]
        "xp3" => crate::xp3::run(rest),
        #[cfg(not(feature = "xp3"))]
        "xp3" => Err(ToolError::Usage(
            "the `xp3` family needs the `xp3` feature".to_string(),
        )),
        #[cfg(feature = "tjsb")]
        "tjsb" => crate::tjsb::run(rest),
        #[cfg(not(feature = "tjsb"))]
        "tjsb" => Err(ToolError::Usage(
            "the `tjsb` family needs the `tjsb` feature".to_string(),
        )),
        #[cfg(feature = "tjs-compile")]
        "tjs" => crate::tjscompile::run(rest),
        #[cfg(not(feature = "tjs-compile"))]
        "tjs" => Err(ToolError::Usage(
            "the `tjs` family needs the `tjs-compile` feature".to_string(),
        )),
        other => Err(ToolError::Usage(format!(
            "unknown command family `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that takes one or two positional values, an output path and
    /// a flag.
    const SPEC: CommandSpec = CommandSpec {
        min_positionals: 1,
        max_positionals: 2,
        options: &[
            OptionSpec {
                key: "output",
                spellings: &["-o", "--output"],
                takes_value: true,
            },
            OptionSpec {
                key: "verbose",
                spellings: &["--verbose"],
                takes_value: false,
            },
        ],
    };

    /// The command line of a successful call.
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn a_command_line_yields_its_positionals_and_options() {
        let invocation = parse(
            &argv(&["archive.xp3", "name", "-o", "out.bin", "--verbose"]),
            &SPEC,
        )
        .expect("the command line parses");
        assert_eq!(invocation.positional(0), Some("archive.xp3"));
        assert_eq!(invocation.positional(1), Some("name"));
        assert_eq!(invocation.positional(2), None);
        assert_eq!(invocation.value("output"), Some("out.bin"));
        assert!(invocation.has_flag("verbose"));
        assert!(!invocation.has_flag("output"));
    }

    #[test]
    fn an_option_value_may_follow_its_name_with_an_equals_sign() {
        let invocation = parse(&argv(&["archive.xp3", "--output=out.bin"]), &SPEC)
            .expect("the command line parses");
        assert_eq!(invocation.value("output"), Some("out.bin"));
    }

    #[test]
    fn a_double_dash_ends_the_option_list() {
        let invocation =
            parse(&argv(&["archive.xp3", "--", "-o"]), &SPEC).expect("the command line parses");
        assert_eq!(invocation.positional(1), Some("-o"));
        assert_eq!(invocation.value("output"), None);
    }

    #[test]
    fn an_unknown_option_is_a_usage_failure() {
        let error = parse(&argv(&["archive.xp3", "--nope"]), &SPEC)
            .expect_err("the command line is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
    }

    #[test]
    fn an_option_without_its_value_is_a_usage_failure() {
        let error =
            parse(&argv(&["archive.xp3", "-o"]), &SPEC).expect_err("the command line is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
    }

    #[test]
    fn a_repeated_option_is_a_usage_failure() {
        let error = parse(&argv(&["archive.xp3", "-o", "a", "-o", "b"]), &SPEC)
            .expect_err("the command line is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
    }

    #[test]
    fn a_wrong_number_of_positional_values_is_a_usage_failure() {
        let error = parse(&argv(&[]), &SPEC).expect_err("the command line is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
        let error = parse(&argv(&["a", "b", "c"]), &SPEC).expect_err("the command line is refused");
        assert!(matches!(&error, ToolError::Usage(_)), "{error:?}");
    }

    #[test]
    fn a_usage_failure_exits_with_two_and_every_other_failure_with_one() {
        assert_eq!(ToolError::Usage("no".to_string()).exit_code(), 2);
        assert_eq!(
            ToolError::Host {
                path: std::path::PathBuf::from("x"),
                source: std::io::Error::other("no"),
            }
            .exit_code(),
            1
        );
    }
}
