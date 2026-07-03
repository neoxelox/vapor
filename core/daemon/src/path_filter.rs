use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use vapor_shared::constants;

use crate::logging;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPathFilterOptions {
    pub use_gitignore: bool,
    pub use_vaporignore: bool,
    pub pre_user_rules: Vec<String>,
    pub post_user_rules: Vec<String>,
}

impl Default for EventPathFilterOptions {
    fn default() -> Self {
        Self {
            use_gitignore: true,
            use_vaporignore: true,
            pre_user_rules: constants::filtering::DEFAULT_PRE_IGNORE_RULES
                .iter()
                .map(|rule| (*rule).to_string())
                .collect(),
            post_user_rules: Vec::new(),
        }
    }
}

impl EventPathFilterOptions {
    pub fn from_process_environment() -> Self {
        Self::from_environment_and_config(&vapor_shared::config::VaporConfig::default())
    }

    /// Resolves the filter options with the canonical precedence:
    /// `VAPOR_*` environment variable → `vapor.json` value → compiled
    /// default (already baked into [`vapor_shared::config::VaporConfig`]).
    pub fn from_environment_and_config(config: &vapor_shared::config::VaporConfig) -> Self {
        Self::resolve(
            env::var(constants::env::VAPOR_USE_GITIGNORE)
                .ok()
                .as_deref(),
            env::var(constants::env::VAPOR_USE_VAPORIGNORE)
                .ok()
                .as_deref(),
            env::var(constants::env::VAPOR_PRE_IGNORE_RULES)
                .ok()
                .as_deref(),
            env::var(constants::env::VAPOR_POST_IGNORE_RULES)
                .ok()
                .as_deref(),
            config,
        )
    }

    fn resolve(
        env_use_gitignore: Option<&str>,
        env_use_vaporignore: Option<&str>,
        env_pre_rules: Option<&str>,
        env_post_rules: Option<&str>,
        config: &vapor_shared::config::VaporConfig,
    ) -> Self {
        let use_gitignore = env_use_gitignore
            .and_then(parse_bool_flag)
            .unwrap_or(config.use_git_ignore);
        let use_vaporignore = env_use_vaporignore
            .and_then(parse_bool_flag)
            .unwrap_or(config.use_vapor_ignore);
        let pre_user_rules = match env_pre_rules {
            Some(raw) => parse_rule_lines(raw),
            None => parse_rule_lines(&config.pre_ignore_rules),
        };
        let post_user_rules = match env_post_rules {
            Some(raw) => parse_rule_lines(raw),
            None => parse_rule_lines(&config.post_ignore_rules),
        };

        Self {
            use_gitignore,
            use_vaporignore,
            pre_user_rules,
            post_user_rules,
        }
    }
}

fn parse_rule_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuleAction {
    Ignore,
    Allow,
}

#[derive(Debug)]
struct CompiledRule {
    action: RuleAction,
    matchers: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn matches(&self, relative_path: &str) -> bool {
        self.matchers
            .iter()
            .any(|matcher| matcher.is_match(relative_path))
    }
}

#[derive(Debug)]
pub struct EventPathFilter {
    watch_root: PathBuf,
    rules: Vec<CompiledRule>,
}

impl EventPathFilter {
    pub fn for_watch_root(watch_root: &Path, options: &EventPathFilterOptions) -> Self {
        let mut rules = Vec::new();

        append_user_rules(&mut rules, &options.pre_user_rules);

        let mut gitignore_file_count = 0usize;
        let mut vaporignore_file_count = 0usize;

        if options.use_gitignore {
            gitignore_file_count = append_rules_from_file_tree(
                &mut rules,
                watch_root,
                constants::filtering::GIT_IGNORE_FILE_NAME,
            );
        }

        if options.use_vaporignore {
            vaporignore_file_count = append_rules_from_file_tree(
                &mut rules,
                watch_root,
                constants::filtering::VAPOR_IGNORE_FILE_NAME,
            );
        }

        append_user_rules(&mut rules, &options.post_user_rules);

        logging::info(
            "Initialized filesystem path filter",
            &[
                ("watch_root", watch_root.display().to_string()),
                ("rule_count", rules.len().to_string()),
                ("use_gitignore", options.use_gitignore.to_string()),
                ("use_vaporignore", options.use_vaporignore.to_string()),
                (
                    "pre_user_rule_count",
                    options.pre_user_rules.len().to_string(),
                ),
                (
                    "post_user_rule_count",
                    options.post_user_rules.len().to_string(),
                ),
                ("gitignore_file_count", gitignore_file_count.to_string()),
                ("vaporignore_file_count", vaporignore_file_count.to_string()),
            ],
        );

        Self {
            watch_root: watch_root.to_path_buf(),
            rules,
        }
    }

    pub fn should_ignore(&self, path: &Path) -> bool {
        let Ok(relative_path) = path.strip_prefix(&self.watch_root) else {
            return false;
        };

        let normalized_relative_path = normalize_relative_path(relative_path);
        if normalized_relative_path.is_empty() {
            return false;
        }

        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(&normalized_relative_path) {
                ignored = matches!(rule.action, RuleAction::Ignore);
            }
        }

        ignored
    }
}

fn append_rules_from_file_tree(
    rules: &mut Vec<CompiledRule>,
    watch_root: &Path,
    file_name: &str,
) -> usize {
    let ignore_files = collect_ignore_files(watch_root, file_name);
    for ignore_file in &ignore_files {
        append_rules_from_file(rules, watch_root, ignore_file.as_path());
    }

    ignore_files.len()
}

fn collect_ignore_files(watch_root: &Path, file_name: &str) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    let mut pending = vec![watch_root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                logging::warning(
                    "Failed to enumerate directory while discovering ignore files; skipping",
                    &[
                        ("directory", directory.display().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    logging::warning(
                        "Failed to read directory entry while discovering ignore files; skipping",
                        &[("error", error.to_string())],
                    );
                    continue;
                }
            };

            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    logging::warning(
                        "Failed to inspect directory entry type while discovering ignore files; skipping",
                        &[
                            ("path", path.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    continue;
                }
            };

            if file_type.is_dir() {
                if is_heavy_ignore_discovery_skip_dir(path.file_name().and_then(|n| n.to_str())) {
                    continue;
                }
                pending.push(path);
                continue;
            }

            if file_type.is_file()
                && path.file_name().and_then(|name| name.to_str()) == Some(file_name)
            {
                discovered.push(path);
            }
        }
    }

    discovered.sort_by(|left, right| {
        let left_depth = left.components().count();
        let right_depth = right.components().count();
        left_depth
            .cmp(&right_depth)
            .then_with(|| left.as_os_str().cmp(right.as_os_str()))
    });
    discovered
}

/// Directory-name matcher used to skip heavy subtrees during startup ignore-file
/// discovery. This is intentionally narrow and mirrors the most impactful
/// entries from `DEFAULT_PRE_IGNORE_RULES`; matched subtrees are still covered
/// by the default ignore rules, so skipping them here only avoids a multi-minute
/// directory walk on massive dependency folders, not correctness.
fn is_heavy_ignore_discovery_skip_dir(name: Option<&str>) -> bool {
    const SKIP_DIRS: &[&str] = &[
        ".git",
        ".npm",
        ".parcel-cache",
        ".pnpm-store",
        ".turbo",
        ".vite",
        ".yarn",
        "build",
        "coverage",
        "dist",
        "node_modules",
        "out",
        "storybook-static",
        "target",
    ];
    let Some(name) = name else {
        return false;
    };
    SKIP_DIRS.contains(&name)
}

fn append_rules_from_file(rules: &mut Vec<CompiledRule>, watch_root: &Path, path: &Path) {
    if !path.exists() {
        return;
    }

    let relative_parent = path
        .parent()
        .and_then(|parent| parent.strip_prefix(watch_root).ok())
        .unwrap_or_else(|| Path::new(""));

    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) => {
            logging::warning(
                "Failed to read ignore rules file; skipping",
                &[
                    ("path", path.display().to_string()),
                    ("error", error.to_string()),
                ],
            );
            return;
        }
    };

    for (index, line) in contents.lines().enumerate() {
        append_rule_line(
            rules,
            line,
            "file",
            Some(path),
            Some(index + 1),
            Some(relative_parent),
        );
    }
}

fn append_user_rules(rules: &mut Vec<CompiledRule>, user_rules: &[String]) {
    for rule in user_rules {
        append_rule_line(rules, rule, "user", None, None, None);
    }
}

fn append_rule_line(
    rules: &mut Vec<CompiledRule>,
    raw_line: &str,
    source: &str,
    source_path: Option<&Path>,
    line_number: Option<usize>,
    base_directory: Option<&Path>,
) {
    let Some((action, pattern)) = parse_rule_line(raw_line) else {
        return;
    };

    match compile_rule(action, pattern.as_str(), base_directory) {
        Ok(rule) => rules.push(rule),
        Err(error) => {
            let mut metadata = vec![
                ("source", source.to_string()),
                ("pattern", pattern),
                ("error", error),
            ];
            if let Some(path) = source_path {
                metadata.push(("path", path.display().to_string()));
            }
            if let Some(line_number) = line_number {
                metadata.push(("line", line_number.to_string()));
            }
            if let Some(base_directory) = base_directory {
                metadata.push(("base_directory", base_directory.display().to_string()));
            }

            logging::warning("Skipping invalid ignore rule", &metadata);
        }
    }
}

fn parse_rule_line(raw_line: &str) -> Option<(RuleAction, String)> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(pattern) = trimmed.strip_prefix("\\!") {
        return Some((RuleAction::Ignore, format!("!{pattern}")));
    }

    if let Some(pattern) = trimmed.strip_prefix("\\#") {
        return Some((RuleAction::Ignore, format!("#{pattern}")));
    }

    if trimmed.starts_with('#') {
        return None;
    }

    if let Some(unignore_pattern) = trimmed.strip_prefix('!') {
        let unignore_pattern = unignore_pattern.trim();
        if unignore_pattern.is_empty() {
            return None;
        }
        return Some((RuleAction::Allow, unignore_pattern.to_string()));
    }

    Some((RuleAction::Ignore, trimmed.to_string()))
}

fn compile_rule(
    action: RuleAction,
    raw_pattern: &str,
    base_directory: Option<&Path>,
) -> Result<CompiledRule, String> {
    let patterns = expand_glob_patterns(raw_pattern, base_directory)?;
    let mut matchers = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        let matcher = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| error.to_string())?
            .compile_matcher();
        matchers.push(matcher);
    }

    Ok(CompiledRule { action, matchers })
}

fn expand_glob_patterns(
    raw_pattern: &str,
    base_directory: Option<&Path>,
) -> Result<Vec<String>, String> {
    let anchored = raw_pattern.starts_with('/');
    let directory_only = raw_pattern.ends_with('/');

    let mut body = raw_pattern;
    if anchored {
        body = body.trim_start_matches('/');
    }
    if directory_only {
        body = body.trim_end_matches('/');
    }

    if body.is_empty() {
        return Err("ignore pattern is empty".to_string());
    }

    let has_separator = body.contains('/');

    let mut stems = if anchored || has_separator {
        vec![body.to_string()]
    } else {
        vec![body.to_string(), format!("**/{body}")]
    };
    stems.sort();
    stems.dedup();

    let mut patterns = Vec::new();
    for stem in stems {
        let mut resolved = stem.clone();
        if let Some(base_directory) = base_directory {
            let base_directory = normalize_relative_path(base_directory);
            if !base_directory.is_empty() {
                resolved = format!("{base_directory}/{resolved}");
            }
        }

        patterns.push(resolved.clone());
        if directory_only {
            patterns.push(format!("{resolved}/**"));
        }
    }

    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn parse_bool_flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_ignore_common_cache_and_build_paths() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter =
            EventPathFilter::for_watch_root(&watch_root, &EventPathFilterOptions::default());

        assert!(filter.should_ignore(&watch_root.join(".git/config")));
        assert!(filter.should_ignore(&watch_root.join("node_modules/pkg/index.js")));
        assert!(filter.should_ignore(&watch_root.join("coverage/unit.json")));
        assert!(!filter.should_ignore(&watch_root.join("src/main.rs")));
    }

    #[test]
    fn gitignore_rules_can_be_disabled() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::write(watch_root.join(".gitignore"), "generated/\n")
            .expect("failed to write .gitignore");

        let filter_with_gitignore =
            EventPathFilter::for_watch_root(&watch_root, &EventPathFilterOptions::default());
        assert!(filter_with_gitignore.should_ignore(&watch_root.join("generated/file.txt")));

        let filter_without_gitignore = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                ..EventPathFilterOptions::default()
            },
        );
        assert!(!filter_without_gitignore.should_ignore(&watch_root.join("generated/file.txt")));
    }

    #[test]
    fn vaporignore_rules_can_be_disabled() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::write(watch_root.join(".vaporignore"), "scratch/\n")
            .expect("failed to write .vaporignore");

        let filter_with_vaporignore =
            EventPathFilter::for_watch_root(&watch_root, &EventPathFilterOptions::default());
        assert!(filter_with_vaporignore.should_ignore(&watch_root.join("scratch/file.txt")));

        let filter_without_vaporignore = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_vaporignore: false,
                ..EventPathFilterOptions::default()
            },
        );
        assert!(!filter_without_vaporignore.should_ignore(&watch_root.join("scratch/file.txt")));
    }

    #[test]
    fn vaporignore_overrides_gitignore_and_pre_user_rules() {
        let (_watch_root_guard, watch_root) = create_test_directory();

        fs::write(
            watch_root.join(".gitignore"),
            "!coverage/from-gitignore.txt\n",
        )
        .expect("failed to write .gitignore");
        fs::write(
            watch_root.join(".vaporignore"),
            "coverage/\n!coverage/from-vaporignore.txt\n",
        )
        .expect("failed to write .vaporignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: true,
                pre_user_rules: vec![
                    "!coverage/from-user-rule.txt".to_string(),
                    "coverage/from-vaporignore.txt".to_string(),
                ],
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );

        assert!(filter.should_ignore(&watch_root.join("coverage/regular.txt")));
        assert!(filter.should_ignore(&watch_root.join("coverage/from-gitignore.txt")));
        assert!(!filter.should_ignore(&watch_root.join("coverage/from-vaporignore.txt")));
        assert!(filter.should_ignore(&watch_root.join("coverage/from-user-rule.txt")));
    }

    #[test]
    fn post_user_rules_override_vaporignore_rules() {
        let (_watch_root_guard, watch_root) = create_test_directory();

        fs::write(
            watch_root.join(".vaporignore"),
            "coverage/\n!coverage/keep.txt\n",
        )
        .expect("failed to write .vaporignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                pre_user_rules: vec!["!coverage/keep.txt".to_string()],
                post_user_rules: vec!["coverage/keep.txt".to_string()],
                ..EventPathFilterOptions::default()
            },
        );

        assert!(filter.should_ignore(&watch_root.join("coverage/keep.txt")));
    }

    #[test]
    fn ignore_file_discovery_skips_heavy_dependency_directories() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::create_dir_all(watch_root.join("node_modules/pkg/deep"))
            .expect("failed to create node_modules tree");
        fs::write(
            watch_root.join("node_modules/pkg/deep/.gitignore"),
            "leaked-rule/\n",
        )
        .expect("failed to write .gitignore inside node_modules");
        fs::create_dir_all(watch_root.join("src")).expect("failed to create src dir");
        fs::write(watch_root.join("src/.gitignore"), "real-rule/\n")
            .expect("failed to write src .gitignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_vaporignore: false,
                pre_user_rules: Vec::new(),
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );

        assert!(filter.should_ignore(&watch_root.join("src/real-rule/something.txt")));
        assert!(!filter.should_ignore(&watch_root.join("other/leaked-rule/something.txt")));
    }

    #[test]
    fn recursively_loads_nested_gitignore_files() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::create_dir_all(watch_root.join("apps/web")).expect("failed to create nested directory");
        fs::write(watch_root.join("apps/web/.gitignore"), "generated/\n")
            .expect("failed to write nested .gitignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_vaporignore: false,
                pre_user_rules: Vec::new(),
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );

        assert!(filter.should_ignore(&watch_root.join("apps/web/generated/app.js")));
        assert!(!filter.should_ignore(&watch_root.join("generated/app.js")));
    }

    #[test]
    fn recursively_loads_nested_vaporignore_files() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::create_dir_all(watch_root.join("apps/desktop"))
            .expect("failed to create nested directory");
        fs::write(watch_root.join("apps/desktop/.vaporignore"), "scratch/\n")
            .expect("failed to write nested .vaporignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                pre_user_rules: Vec::new(),
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );

        assert!(filter.should_ignore(&watch_root.join("apps/desktop/scratch/file.txt")));
        assert!(!filter.should_ignore(&watch_root.join("scratch/file.txt")));
    }

    #[test]
    fn pre_user_rules_accept_gitignore_style_comments_and_unignore() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                use_vaporignore: false,
                pre_user_rules: vec![
                    "# comment".to_string(),
                    "tmp/".to_string(),
                    "!tmp/keep.txt".to_string(),
                    "\\#literal-file".to_string(),
                ],
                post_user_rules: Vec::new(),
            },
        );

        assert!(filter.should_ignore(&watch_root.join("tmp/a.txt")));
        assert!(!filter.should_ignore(&watch_root.join("tmp/keep.txt")));
        assert!(filter.should_ignore(&watch_root.join("#literal-file")));
    }

    #[test]
    fn empty_pre_user_rules_disable_default_ignore_set() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                pre_user_rules: Vec::new(),
                post_user_rules: Vec::new(),
                ..EventPathFilterOptions::default()
            },
        );

        assert!(!filter.should_ignore(&watch_root.join("node_modules/pkg/index.js")));
    }

    #[test]
    fn options_default_to_config_values_when_environment_is_unset() {
        let config = vapor_shared::config::VaporConfig {
            use_git_ignore: false,
            use_vapor_ignore: true,
            pre_ignore_rules: "tmp/\n*.cache".to_string(),
            post_ignore_rules: "keep-out/".to_string(),
            ..vapor_shared::config::VaporConfig::default()
        };

        let options = EventPathFilterOptions::resolve(None, None, None, None, &config);

        assert!(!options.use_gitignore);
        assert!(options.use_vaporignore);
        assert_eq!(options.pre_user_rules, vec!["tmp/", "*.cache"]);
        assert_eq!(options.post_user_rules, vec!["keep-out/"]);
    }

    #[test]
    fn environment_variables_override_config_values() {
        let config = vapor_shared::config::VaporConfig {
            use_git_ignore: true,
            pre_ignore_rules: "from-config/".to_string(),
            ..vapor_shared::config::VaporConfig::default()
        };

        let options = EventPathFilterOptions::resolve(
            Some("false"),
            Some("invalid-falls-back-to-config"),
            Some("from-env/\n\n"),
            None,
            &config,
        );

        assert!(!options.use_gitignore, "env override wins");
        assert!(
            options.use_vaporignore,
            "unparseable env value falls back to config"
        );
        assert_eq!(options.pre_user_rules, vec!["from-env/"]);
        assert!(options.post_user_rules.is_empty());
    }

    #[test]
    fn default_config_yields_the_compiled_default_rule_set() {
        let options = EventPathFilterOptions::resolve(
            None,
            None,
            None,
            None,
            &vapor_shared::config::VaporConfig::default(),
        );
        assert!(options.use_gitignore);
        assert!(options.use_vaporignore);
        assert!(
            options
                .pre_user_rules
                .iter()
                .any(|rule| rule == "node_modules/")
        );
        assert!(options.post_user_rules.is_empty());
    }

    #[test]
    fn parses_boolean_environment_flags() {
        assert_eq!(parse_bool_flag("true"), Some(true));
        assert_eq!(parse_bool_flag("YES"), Some(true));
        assert_eq!(parse_bool_flag("0"), Some(false));
        assert_eq!(parse_bool_flag("off"), Some(false));
        assert_eq!(parse_bool_flag("invalid"), None);
    }

    /// Owns the `TempDir` guard so the directory lives for the test and
    /// is removed automatically afterwards, per the testing-strategy
    /// mandate that every integration-style test uses its own
    /// `tempfile::TempDir`.
    fn create_test_directory() -> (TempDir, PathBuf) {
        let guard = TempDir::new().expect("failed to create test directory");
        let root = guard.path().to_path_buf();
        (guard, root)
    }
}
