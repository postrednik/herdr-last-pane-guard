use std::{fs, io::ErrorKind, path::Path};

use serde::Serialize;

use crate::{log, Error, Result};

#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub restore_workspace: String,
    pub restore_tab: String,
    pub loop_guard_seconds: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            restore_workspace: "ask".into(),
            restore_tab: "ask".into(),
            loop_guard_seconds: 5,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut config = Self::default();
        if path.as_os_str().is_empty() {
            return Err(Error::Invalid("cannot read configuration file"));
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(config),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        if !fs::metadata(path)?.is_file() {
            return Err(Error::Invalid("cannot read configuration file"));
        }
        let contents = fs::read(path)?;
        let contents = String::from_utf8_lossy(&contents);
        for line in contents.split('\n') {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                log("ignored malformed configuration line.");
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "RESTORE_WORKSPACE" => {
                    config.restore_workspace =
                        mode(value, "invalid RESTORE_WORKSPACE; using default.");
                }
                "RESTORE_TAB" => {
                    config.restore_tab = mode(value, "invalid RESTORE_TAB; using default.");
                }
                "LOOP_GUARD_SECONDS" => {
                    config.loop_guard_seconds = match seconds(value) {
                        Some(seconds) => seconds,
                        None => {
                            log("invalid LOOP_GUARD_SECONDS; using default.");
                            5
                        }
                    };
                }
                _ => log("ignored unknown configuration key."),
            }
        }
        Ok(config)
    }
}

fn mode(value: &str, warning: &'static str) -> String {
    match value {
        "ask" | "auto" | "off" => value.into(),
        _ => {
            log(warning);
            "ask".into()
        }
    }
}

fn seconds(value: &str) -> Option<i64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let significant = value.trim_start_matches('0');
    if significant.is_empty() {
        return Some(0);
    }
    significant
        .parse::<i64>()
        .ok()
        .filter(|number| *number <= i64::from(i32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn load(contents: &str) -> Config {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("config.env");
        fs::write(&path, contents).unwrap();
        Config::load(&path).unwrap()
    }

    fn assert_config(config: &Config, workspace: &str, tab: &str, seconds: i64) {
        assert_eq!(config.restore_workspace, workspace);
        assert_eq!(config.restore_tab, tab);
        assert_eq!(config.loop_guard_seconds, seconds);
    }

    #[test]
    fn missing_empty_and_comments_use_defaults() {
        let directory = TempDir::new().unwrap();
        let config = Config::load(&directory.path().join("missing.env")).unwrap();
        assert_config(&config, "ask", "ask", 5);
        for input in ["", "\n \t\n# comment\n \t# comment\n"] {
            assert_config(&load(input), "ask", "ask", 5);
        }
    }

    #[test]
    fn accepts_modes_and_decimal_seconds() {
        for mode in ["ask", "auto", "off"] {
            let config = load(&format!("RESTORE_WORKSPACE={mode}\nRESTORE_TAB={mode}\n"));
            assert_config(&config, mode, mode, 5);
        }
        for (input, expected) in [
            ("0", 0),
            ("0000", 0),
            ("5", 5),
            ("0005", 5),
            ("0008", 8),
            ("0009", 9),
            ("00010", 10),
            ("2147483647", 2147483647),
            ("0002147483647", 2147483647),
        ] {
            assert_config(
                &load(&format!("LOOP_GUARD_SECONDS={input}\n")),
                "ask",
                "ask",
                expected,
            );
        }
    }

    #[test]
    fn invalid_values_reset_previous_valid_values() {
        for value in [
            "",
            "ASK",
            "Auto",
            "false",
            "\"auto\"",
            "'off'",
            "auto # comment",
            "ask=auto",
            "\\auto",
        ] {
            let config = load(&format!(
                "RESTORE_WORKSPACE=auto\nRESTORE_TAB=off\nRESTORE_WORKSPACE={value}\nRESTORE_TAB={value}\n"
            ));
            assert_config(&config, "ask", "ask", 5);
        }
        for value in [
            "",
            "-1",
            "+1",
            "1.0",
            "0x10",
            "1e3",
            "1 2",
            "1=2",
            "2147483648",
            "2147483649",
            "9999999999",
            "10000000000",
        ] {
            let config = load(&format!(
                "LOOP_GUARD_SECONDS=19\nLOOP_GUARD_SECONDS={value}\n"
            ));
            assert_config(&config, "ask", "ask", 5);
        }
    }

    #[test]
    fn non_utf8_values_reset_previous_assignments() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("config.env");
        fs::write(&path, b"RESTORE_WORKSPACE=auto\nRESTORE_WORKSPACE=\xff\n").unwrap();
        assert_config(&Config::load(&path).unwrap(), "ask", "ask", 5);
    }

    #[test]
    fn large_decimal_inputs_do_not_overflow() {
        let zeroes = "0".repeat(2000);
        assert_eq!(
            load(&format!("LOOP_GUARD_SECONDS={}\n", "9".repeat(2000))).loop_guard_seconds,
            5
        );
        assert_eq!(
            load(&format!("LOOP_GUARD_SECONDS={zeroes}9\n")).loop_guard_seconds,
            9
        );
        assert_eq!(
            load(&format!("LOOP_GUARD_SECONDS={zeroes}\n")).loop_guard_seconds,
            0
        );
    }

    #[test]
    fn last_assignment_wins_and_each_load_resets_defaults() {
        let config = load("RESTORE_WORKSPACE=off\nRESTORE_WORKSPACE=auto\nRESTORE_TAB=auto\nRESTORE_TAB=off\nLOOP_GUARD_SECONDS=8\nLOOP_GUARD_SECONDS=0009\n");
        assert_config(&config, "auto", "off", 9);
        assert_config(&load(""), "ask", "ask", 5);
    }

    #[test]
    fn accepts_crlf_whitespace_and_unterminated_lines() {
        let config = load("\r\n \t# comment\r\n \tRESTORE_WORKSPACE \t= \tauto \t\r\nRESTORE_TAB = off\r\n LOOP_GUARD_SECONDS = 0005 \r");
        assert_config(&config, "auto", "off", 5);
        assert_config(&load("RESTORE_WORKSPACE=off"), "off", "ask", 5);
    }

    #[test]
    fn unknown_and_malformed_lines_are_inert() {
        let config = load("PATH=not-a-path\nUNKNOWN=changed\nmalformed\nexport RESTORE_WORKSPACE=auto\n=private\nRESTORE_TAB auto\n");
        assert_config(&config, "ask", "ask", 5);
    }

    #[test]
    fn shell_syntax_is_never_executed() {
        let directory = TempDir::new().unwrap();
        let marker = directory.path().join("executed");
        let config = load(&format!(
            "RESTORE_WORKSPACE=auto\nRESTORE_TAB=off\nLOOP_GUARD_SECONDS=19\nRESTORE_WORKSPACE=$(touch '{}')\nRESTORE_TAB=`touch '{}'`\nLOOP_GUARD_SECONDS=1; touch '{}'\nUNKNOWN=$(touch '{}')\n",
            marker.display(), marker.display(), marker.display(), marker.display(),
        ));
        assert_config(&config, "ask", "ask", 5);
        assert!(!marker.exists());
    }

    #[test]
    fn rejects_empty_path_and_directories() {
        let directory = TempDir::new().unwrap();
        assert!(Config::load(Path::new("")).is_err());
        assert!(Config::load(directory.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn follows_regular_symlinks_but_rejects_dangling_and_nonregular_files() {
        use std::os::unix::{fs::symlink, net::UnixListener};

        let directory = TempDir::new().unwrap();
        let target = directory.path().join("config.env");
        fs::write(&target, "RESTORE_WORKSPACE=auto").unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();
        assert_config(&Config::load(&link).unwrap(), "auto", "ask", 5);
        let dangling = directory.path().join("dangling");
        symlink(directory.path().join("missing"), &dangling).unwrap();
        assert!(Config::load(&dangling).is_err());
        let socket = directory.path().join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert!(Config::load(&socket).is_err());
    }
}
