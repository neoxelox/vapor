use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
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
    globs: Vec<Glob>,
}

#[derive(Debug)]
pub struct EventPathFilter {
    watch_root: PathBuf,
    rules: Vec<CompiledRule>,
    /// All rule globs compiled into one automaton for O(1)-per-event
    /// matching on the fs-watch callback thread; `glob_owner[i]` is the
    /// index of the rule that contributed the i-th glob so last-match-wins
    /// (highest matching rule index) still holds.
    glob_set: GlobSet,
    glob_owner: Vec<usize>,
}

/// Compiles every rule's globs into a single [`GlobSet`], recording which
/// rule each glob belongs to.
fn build_glob_set(rules: &[CompiledRule]) -> (GlobSet, Vec<usize>) {
    let mut builder = GlobSetBuilder::new();
    let mut glob_owner = Vec::new();
    for (rule_index, rule) in rules.iter().enumerate() {
        for glob in &rule.globs {
            builder.add(glob.clone());
            glob_owner.push(rule_index);
        }
    }
    // Every glob already compiled individually in compile_rule, so the set
    // build cannot fail; fall back to an empty set defensively.
    let glob_set = builder.build().unwrap_or_else(|_| GlobSet::empty());
    (glob_set, glob_owner)
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

        let (glob_set, glob_owner) = build_glob_set(&rules);
        Self {
            watch_root: watch_root.to_path_buf(),
            rules,
            glob_set,
            glob_owner,
        }
    }

    pub fn should_ignore(&self, path: &Path) -> bool {
        // Engine-internal artifacts (atomic-write temp files, op-id
        // side-files) are unconditionally invisible: loop prevention
        // depends on them never becoming intents, so no user rule may
        // re-include them.
        if is_internal_artifact(path) {
            return true;
        }

        let Ok(relative_path) = path.strip_prefix(&self.watch_root) else {
            return false;
        };
        // Vapor's own directories are invisible with everything under
        // them, so a runtime directory or a volume's trash inside the
        // watch root never becomes intents.
        if relative_path.components().any(|component| {
            component.as_os_str().to_str().is_some_and(|name| {
                constants::filtering::INTERNAL_IGNORE_DIRECTORY_NAMES.contains(&name)
            })
        }) {
            return true;
        }

        let normalized_relative_path = normalize_relative_path(relative_path);
        if normalized_relative_path.is_empty() {
            return false;
        }

        // One automaton pass returns the matching glob indices; the
        // highest-indexed rule among them wins (last-match-wins).
        let winning_rule = self
            .glob_set
            .matches(&normalized_relative_path)
            .into_iter()
            .map(|glob_index| self.glob_owner[glob_index])
            .max();
        match winning_rule {
            Some(rule_index) => matches!(self.rules[rule_index].action, RuleAction::Ignore),
            None => false,
        }
    }
}

/// Whether the file name marks a Vapor-internal artifact
/// (`constants::filtering::INTERNAL_IGNORE_FILE_*`).
fn is_internal_artifact(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    constants::filtering::INTERNAL_IGNORE_FILE_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || constants::filtering::INTERNAL_IGNORE_FILE_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
        || constants::filtering::INTERNAL_IGNORE_DIRECTORY_NAMES.contains(&name)
}

fn append_rules_from_file_tree(
    rules: &mut Vec<CompiledRule>,
    watch_root: &Path,
    file_name: &str,
) -> usize {
    // Mirror git: never read an ignore file inside a directory that the
    // rules compiled so far already exclude. Otherwise a third-party
    // ignore file (e.g. a vendored `.gitignore` with a `!keep` negation)
    // inside a user-excluded `vendor/` could re-include content the user
    // opted out of, and the walk would descend huge excluded trees.
    let (glob_set, glob_owner) = build_glob_set(rules);
    let is_ignored_dir =
        |dir: &Path| evaluate_rules(rules, &glob_set, &glob_owner, watch_root, dir);
    let ignore_files = collect_ignore_files(watch_root, file_name, &is_ignored_dir);
    for ignore_file in &ignore_files {
        append_rules_from_file(rules, watch_root, ignore_file.as_path());
    }

    ignore_files.len()
}

/// Evaluates the compiled rules against `path` (last-match-wins), used to
/// prune already-excluded directories during ignore-file discovery.
fn evaluate_rules(
    rules: &[CompiledRule],
    glob_set: &GlobSet,
    glob_owner: &[usize],
    watch_root: &Path,
    path: &Path,
) -> bool {
    let Ok(relative_path) = path.strip_prefix(watch_root) else {
        return false;
    };
    let normalized = normalize_relative_path(relative_path);
    if normalized.is_empty() {
        return false;
    }
    glob_set
        .matches(&normalized)
        .into_iter()
        .map(|glob_index| glob_owner[glob_index])
        .max()
        .map(|rule_index| matches!(rules[rule_index].action, RuleAction::Ignore))
        .unwrap_or(false)
}

fn collect_ignore_files(
    watch_root: &Path,
    file_name: &str,
    is_ignored_dir: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
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
                // Never descend into (or read ignore files under) a
                // directory the rules already exclude.
                if is_ignored_dir(&path) {
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
/// directory walk on massive dependency folders, not correctness. `.git`
/// is the one exception: it syncs by default (not rule-ignored) but never
/// carries user ignore files, so discovery still skips walking it.
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
    // Git semantics: a non-negated bare pattern (`target`, `/build`) also
    // excludes the directory's whole subtree, not just an entry named
    // `target`. Emit the descendant glob for ignore rules so
    // `target/debug/app.o` is filtered even without a trailing slash. A
    // negation (allow) rule only re-includes the named path, so it does
    // not get the blanket descendant expansion.
    let emit_descendants = matches!(action, RuleAction::Ignore);
    let patterns = expand_glob_patterns(raw_pattern, base_directory, emit_descendants)?;
    let mut globs = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| error.to_string())?;
        globs.push(glob);
    }

    Ok(CompiledRule { action, globs })
}

fn expand_glob_patterns(
    raw_pattern: &str,
    base_directory: Option<&Path>,
    emit_descendants: bool,
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
        if directory_only || emit_descendants {
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

        assert!(filter.should_ignore(&watch_root.join("node_modules/pkg/index.js")));
        assert!(filter.should_ignore(&watch_root.join("coverage/unit.json")));
        assert!(filter.should_ignore(&watch_root.join("target/debug/app")));
        assert!(filter.should_ignore(&watch_root.join("src/__pycache__/mod.pyc")));
        assert!(!filter.should_ignore(&watch_root.join("src/main.rs")));
        // Repositories and dotenv files sync whole by default.
        assert!(!filter.should_ignore(&watch_root.join(".git/config")));
        assert!(!filter.should_ignore(&watch_root.join(".env.local")));
    }

    #[test]
    fn internal_artifacts_are_ignored_unconditionally() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        // Even an explicit user re-include cannot surface engine
        // internals: loop prevention depends on it.
        let options = EventPathFilterOptions {
            pre_user_rules: vec![
                "!.vapor-tmp-*".to_string(),
                "!*.vapor-meta.json".to_string(),
            ],
            ..EventPathFilterOptions::default()
        };
        let filter = EventPathFilter::for_watch_root(&watch_root, &options);

        assert!(filter.should_ignore(&watch_root.join(".vapor-tmp-dl-op42")));
        assert!(filter.should_ignore(&watch_root.join("docs/.vapor-tmp-upload")));
        assert!(filter.should_ignore(&watch_root.join("docs/report.md.vapor-meta.json")));
        assert!(!filter.should_ignore(&watch_root.join("docs/report.md")));
    }

    #[test]
    fn a_vapor_directory_is_invisible_with_everything_under_it() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let options = EventPathFilterOptions {
            pre_user_rules: vec!["!.vapor".to_string(), "!.vapor/**".to_string()],
            ..EventPathFilterOptions::default()
        };
        let filter = EventPathFilter::for_watch_root(&watch_root, &options);

        // The runtime directory or a volume's trash inside the root,
        // at any depth, files included.
        assert!(filter.should_ignore(&watch_root.join(".vapor")));
        assert!(filter.should_ignore(&watch_root.join(".vapor/trash/default/1-0000/keep.txt")));
        assert!(filter.should_ignore(&watch_root.join("projects/app/.vapor/logs/vapord.logs")));
        // The user's ignore file is a different name and still syncs.
        assert!(!filter.should_ignore(&watch_root.join(".vaporignore")));
        assert!(!filter.should_ignore(&watch_root.join("docs/.vapor-notes.md")));
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
    fn ignore_files_inside_user_excluded_directories_do_not_leak_negations() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        // A vendored package ships its own .gitignore with a re-include.
        fs::create_dir_all(watch_root.join("vendor/pkg")).expect("nested dir");
        fs::write(watch_root.join("vendor/pkg/.gitignore"), "!important.txt\n")
            .expect("vendored .gitignore");

        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: true,
                use_vaporignore: false,
                // The user explicitly excludes the whole vendor tree.
                pre_user_rules: vec!["vendor/".to_string()],
                post_user_rules: Vec::new(),
            },
        );

        // The vendored negation must not re-include content under the
        // user-excluded directory: git never reads ignore files there.
        assert!(filter.should_ignore(&watch_root.join("vendor/pkg/important.txt")));
        assert!(filter.should_ignore(&watch_root.join("vendor/pkg/other.txt")));
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
    fn bare_directory_pattern_ignores_the_directory_subtree() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter = EventPathFilter::for_watch_root(
            &watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                use_vaporignore: false,
                // Bare (no trailing slash) directory rules, git-style.
                pre_user_rules: vec!["target".to_string(), "/build".to_string()],
                post_user_rules: Vec::new(),
            },
        );

        // The directory entry itself and its whole subtree are ignored.
        assert!(filter.should_ignore(&watch_root.join("target")));
        assert!(filter.should_ignore(&watch_root.join("target/debug/app.o")));
        assert!(filter.should_ignore(&watch_root.join("crate/target/debug/x.rlib")));
        assert!(filter.should_ignore(&watch_root.join("build/out.bin")));
        // An anchored `build` rule does not match a nested build dir.
        assert!(!filter.should_ignore(&watch_root.join("sub/build/out.bin")));
        assert!(!filter.should_ignore(&watch_root.join("src/main.rs")));
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
