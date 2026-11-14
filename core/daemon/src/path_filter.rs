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
            pre_user_rules: vapor_shared::config::default_pre_ignore_rules(),
            post_user_rules: Vec::new(),
        }
    }
}

impl EventPathFilterOptions {
    /// The filter options `vapor.json` sets (defaults already baked into
    /// [`vapor_shared::config::VaporConfig`]).
    pub fn from_config(config: &vapor_shared::config::VaporConfig) -> Self {
        Self {
            use_gitignore: config.use_gitignore,
            use_vaporignore: config.use_vaporignore,
            pre_user_rules: parse_rule_lines(&config.pre_ignore_rules),
            post_user_rules: parse_rule_lines(&config.post_ignore_rules),
        }
    }
}

fn parse_rule_lines(rules: &[String]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| rule.trim())
        .filter(|rule| !rule.is_empty())
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
    globs: Vec<RuleGlob>,
    /// The rule as it was written (`build/`, `*.log`), for the list of
    /// what the rules hide.
    pattern: String,
}

/// One glob of a rule. A rule that ends in `/` names folders: its
/// `folder_only` glob matches the folder itself and applies to a folder
/// only, so `build/` does not hide a file named `build`. The glob for
/// what lies under the folder (`build/**`) applies to every path.
#[derive(Debug)]
struct RuleGlob {
    glob: Glob,
    folder_only: bool,
}

#[derive(Clone, Copy, Debug)]
struct GlobOwner {
    rule: usize,
    folder_only: bool,
}

#[derive(Debug)]
pub struct EventPathFilter {
    watch_root: PathBuf,
    rules: Vec<CompiledRule>,
    /// All rule globs compiled into one automaton for O(1)-per-event
    /// matching on the fs-watch callback thread; `glob_owner[i]` names
    /// the rule that contributed the i-th glob so last-match-wins
    /// (highest matching rule index) still holds.
    glob_set: GlobSet,
    glob_owner: Vec<GlobOwner>,
}

/// Compiles every rule's globs into a single [`GlobSet`], recording which
/// rule each glob belongs to.
fn build_glob_set(rules: &[CompiledRule]) -> (GlobSet, Vec<GlobOwner>) {
    let mut builder = GlobSetBuilder::new();
    let mut glob_owner = Vec::new();
    for (rule_index, rule) in rules.iter().enumerate() {
        for rule_glob in &rule.globs {
            builder.add(rule_glob.glob.clone());
            glob_owner.push(GlobOwner {
                rule: rule_index,
                folder_only: rule_glob.folder_only,
            });
        }
    }
    // Every glob already compiled individually in compile_rule, so the set
    // build cannot fail; fall back to an empty set defensively.
    let glob_set = builder.build().unwrap_or_else(|_| GlobSet::empty());
    (glob_set, glob_owner)
}

/// The last rule that matches one path: read as a file, and read as a
/// folder. `None` when no rule matches. The two differ only where a
/// folder rule names the path itself.
fn winning_rules(
    glob_set: &GlobSet,
    glob_owner: &[GlobOwner],
    normalized: &str,
) -> (Option<usize>, Option<usize>) {
    let mut as_file: Option<usize> = None;
    let mut as_folder: Option<usize> = None;
    for glob_index in glob_set.matches(normalized) {
        let owner = glob_owner[glob_index];
        as_folder = as_folder.max(Some(owner.rule));
        if !owner.folder_only {
            as_file = as_file.max(Some(owner.rule));
        }
    }
    (as_file, as_folder)
}

/// The rule that takes the path `normalized` (relative to the watch
/// root) out of scope, or `None` when the path is in scope. `is_folder`
/// is asked only when a folder rule names the path itself.
///
/// A negation follows git's rule: a path under an excluded folder
/// cannot be included again. The watcher and the reconcile walk both
/// decide here, so a file is in scope for both or for neither.
fn ignoring_rule(
    rules: &[CompiledRule],
    glob_set: &GlobSet,
    glob_owner: &[GlobOwner],
    normalized: &str,
    is_folder: &dyn Fn() -> bool,
) -> Option<usize> {
    let is_ignore =
        |winner: Option<usize>| winner.filter(|index| rules[*index].action == RuleAction::Ignore);
    let own = match winning_rules(glob_set, glob_owner, normalized) {
        (as_file, as_folder) if as_file == as_folder => as_file,
        (as_file, as_folder) => {
            if is_folder() {
                as_folder
            } else {
                as_file
            }
        }
    };
    // Every ignore rule also matches what lies under the path it names.
    // A path that no rule matches is under no ignored folder.
    let winner = own?;
    if rules[winner].action == RuleAction::Ignore {
        return Some(winner);
    }
    // A negation matched. It includes the path again unless a folder
    // above it is excluded.
    normalized.match_indices('/').find_map(|(end, _)| {
        let (_, as_folder) = winning_rules(glob_set, glob_owner, &normalized[..end]);
        is_ignore(as_folder)
    })
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

    /// Whether `path` is out of scope. For a path whose kind the caller
    /// does not know: the filesystem is asked whether it is a folder,
    /// and only when a folder rule (`build/`) names the path itself. A
    /// path that is not there counts as a file.
    pub fn should_ignore(&self, path: &Path) -> bool {
        self.ignores(path, &|| {
            fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
        })
    }

    /// [`Self::should_ignore`] for a path whose kind the caller knows:
    /// a directory entry it just listed, a cloud entry.
    pub fn should_ignore_kind(&self, path: &Path, is_folder: bool) -> bool {
        self.ignores(path, &|| is_folder)
    }

    fn ignores(&self, path: &Path, is_folder: &dyn Fn() -> bool) -> bool {
        self.is_internal(path) || self.deciding_rule(path, is_folder).is_some()
    }

    /// Engine-internal artifacts (atomic-write temp files, op-id
    /// side-files) and Vapor's own directories with everything under
    /// them. They are unconditionally invisible: loop prevention depends
    /// on them never becoming intents, so no user rule may re-include
    /// them.
    fn is_internal(&self, path: &Path) -> bool {
        if is_internal_artifact(path) {
            return true;
        }
        let Ok(relative_path) = path.strip_prefix(&self.watch_root) else {
            return false;
        };
        relative_path.components().any(|component| {
            component.as_os_str().to_str().is_some_and(|name| {
                constants::filtering::INTERNAL_IGNORE_DIRECTORY_NAMES.contains(&name)
            })
        })
    }

    fn deciding_rule(&self, path: &Path, is_folder: &dyn Fn() -> bool) -> Option<usize> {
        let relative_path = path.strip_prefix(&self.watch_root).ok()?;
        let normalized_relative_path = normalize_relative_path(relative_path);
        if normalized_relative_path.is_empty() {
            return None;
        }
        ignoring_rule(
            &self.rules,
            &self.glob_set,
            &self.glob_owner,
            &normalized_relative_path,
            is_folder,
        )
    }

    /// The rule, as it was written, that keeps `path` out of sync:
    /// `build/`, `*.log`. `None` for a path that syncs, and for one of
    /// Vapor's own files, which no rule decides and no user misses.
    pub fn ignored_by_rule(&self, path: &Path, is_folder: bool) -> Option<&str> {
        if self.is_internal(path) {
            return None;
        }
        self.deciding_rule(path, &|| is_folder)
            .map(|index| self.rules[index].pattern.as_str())
    }

    /// The rules in the order they apply, as text. Two filters with the
    /// same signature decide every path the same way.
    pub fn signature(&self) -> Vec<String> {
        self.rules
            .iter()
            .flat_map(|rule| {
                rule.globs.iter().map(move |rule_glob| {
                    format!(
                        "{}{}{}",
                        match rule.action {
                            RuleAction::Ignore => '-',
                            RuleAction::Allow => '+',
                        },
                        if rule_glob.folder_only { "d:" } else { ":" },
                        rule_glob.glob.glob()
                    )
                })
            })
            .collect()
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
        || constants::filtering::INTERNAL_IGNORE_FILE_NAMES.contains(&name)
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
    let is_ignored_dir = |dir: &Path| {
        let Ok(relative_path) = dir.strip_prefix(watch_root) else {
            return false;
        };
        let normalized = normalize_relative_path(relative_path);
        !normalized.is_empty()
            && ignoring_rule(rules, &glob_set, &glob_owner, &normalized, &|| true).is_some()
    };
    let ignore_files = collect_ignore_files(watch_root, file_name, &is_ignored_dir);
    for ignore_file in &ignore_files {
        append_rules_from_file(rules, watch_root, ignore_file.as_path());
    }

    ignore_files.len()
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

    for (pattern, folder_only) in patterns {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| error.to_string())?;
        globs.push(RuleGlob { glob, folder_only });
    }

    Ok(CompiledRule {
        action,
        globs,
        pattern: match action {
            RuleAction::Ignore => raw_pattern.to_string(),
            RuleAction::Allow => format!("!{raw_pattern}"),
        },
    })
}

/// The glob patterns of one rule, each with whether it applies to a
/// folder only: the pattern for the path itself when the rule ends in
/// `/`.
fn expand_glob_patterns(
    raw_pattern: &str,
    base_directory: Option<&Path>,
    emit_descendants: bool,
) -> Result<Vec<(String, bool)>, String> {
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

        patterns.push((resolved.clone(), directory_only));
        if directory_only || emit_descendants {
            patterns.push((format!("{resolved}/**"), false));
        }
    }

    patterns.sort();
    patterns.dedup();
    Ok(patterns)
}

fn normalize_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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
        assert!(filter.should_ignore(&watch_root.join("projects/app/.vapor/logs/vapord.log")));
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
            "coverage/*\n!coverage/from-vaporignore.txt\n",
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
                    "tmp/*".to_string(),
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

    fn filter_with_rules(watch_root: &Path, rules: &[&str]) -> EventPathFilter {
        EventPathFilter::for_watch_root(
            watch_root,
            &EventPathFilterOptions {
                use_gitignore: false,
                use_vaporignore: false,
                pre_user_rules: rules.iter().map(|rule| rule.to_string()).collect(),
                post_user_rules: Vec::new(),
            },
        )
    }

    #[test]
    fn a_file_under_an_excluded_folder_cannot_be_included_again() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter = filter_with_rules(
            &watch_root,
            &[
                "vendor/",
                "!vendor/keep.txt",
                "logs/*",
                "!logs/keep.txt",
                "cache/",
                "!cache/",
                "!cache/kept/note.txt",
            ],
        );
        // Git's rule: the folder is excluded, so nothing under it comes
        // back. The reconcile walk never enters the folder, and the
        // watcher has to agree with it.
        assert!(filter.should_ignore_kind(&watch_root.join("vendor"), true));
        assert!(filter.should_ignore(&watch_root.join("vendor/keep.txt")));
        assert!(filter.should_ignore(&watch_root.join("vendor/lib/deep/keep.txt")));
        // The way to keep one file: exclude what is in the folder, not
        // the folder.
        assert!(!filter.should_ignore_kind(&watch_root.join("logs"), true));
        assert!(filter.should_ignore(&watch_root.join("logs/today.log")));
        assert!(!filter.should_ignore(&watch_root.join("logs/keep.txt")));
        // A folder that a later rule includes again is not excluded.
        assert!(!filter.should_ignore_kind(&watch_root.join("cache"), true));
        assert!(!filter.should_ignore(&watch_root.join("cache/kept/note.txt")));
    }

    #[test]
    fn a_folder_rule_hides_folders_and_not_a_file_of_that_name() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        fs::create_dir_all(watch_root.join("build")).expect("folder");
        fs::create_dir_all(watch_root.join("notes")).expect("folder");
        fs::write(watch_root.join("notes/build"), b"a file named build").expect("file");
        let filter = filter_with_rules(&watch_root, &["build/", "target"]);

        // The caller that knows the kind says it.
        assert!(filter.should_ignore_kind(&watch_root.join("src/build"), true));
        assert!(!filter.should_ignore_kind(&watch_root.join("src/build"), false));
        // The caller that does not know gets the answer from the disk,
        // and a path that is not there counts as a file.
        assert!(filter.should_ignore(&watch_root.join("build")));
        assert!(!filter.should_ignore(&watch_root.join("notes/build")));
        assert!(!filter.should_ignore(&watch_root.join("gone/build")));
        // What lies under the folder is hidden whatever its kind.
        assert!(filter.should_ignore(&watch_root.join("build/out/app.o")));
        assert!(filter.should_ignore_kind(&watch_root.join("pkg/build/app.o"), false));
        // A rule with no trailing slash names files and folders.
        assert!(filter.should_ignore_kind(&watch_root.join("target"), false));
        assert!(filter.should_ignore_kind(&watch_root.join("target"), true));
    }

    #[test]
    fn the_root_marker_is_hidden_by_its_whole_name_only() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter = filter_with_rules(&watch_root, &[]);
        assert!(filter.should_ignore(&watch_root.join(".vapor-root")));
        assert!(filter.should_ignore(&watch_root.join("volume/.vapor-root")));
        // A file of the user's whose name only starts like the marker.
        assert!(!filter.should_ignore(&watch_root.join(".vapor-roots.txt")));
        assert!(!filter.should_ignore(&watch_root.join("notes/.vapor-rootless")));
    }

    #[test]
    fn the_rule_that_hides_a_path_is_named_as_it_was_written() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let filter =
            filter_with_rules(&watch_root, &["build/", "*.log", "vendor/", "!vendor/keep"]);
        let rule = |path: &str, is_folder: bool| {
            filter
                .ignored_by_rule(&watch_root.join(path), is_folder)
                .map(str::to_string)
        };
        assert_eq!(rule("build", true).as_deref(), Some("build/"));
        assert_eq!(rule("app/build/out.o", false).as_deref(), Some("build/"));
        assert_eq!(rule("notes/today.log", false).as_deref(), Some("*.log"));
        // Under an excluded folder, the folder's rule is the one.
        assert_eq!(rule("vendor/keep", false).as_deref(), Some("vendor/"));
        assert_eq!(rule("notes/today.txt", false), None, "in scope");
        assert_eq!(rule("build", false), None, "a file named build syncs");
        // Vapor's own files are hidden by no rule.
        assert_eq!(rule(".vapor-tmp-upload", false), None);
        assert!(filter.should_ignore(&watch_root.join(".vapor-tmp-upload")));
    }

    #[test]
    fn two_filters_with_the_same_rules_have_the_same_signature() {
        let (_watch_root_guard, watch_root) = create_test_directory();
        let rules = ["build/", "!build/keep", "*.log"];
        let first = filter_with_rules(&watch_root, &rules);
        assert_eq!(
            first.signature(),
            filter_with_rules(&watch_root, &rules).signature()
        );
        assert_ne!(
            first.signature(),
            filter_with_rules(&watch_root, &["build", "!build/keep", "*.log"]).signature(),
            "`build` and `build/` decide a file named build two ways"
        );
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
    fn options_come_from_the_config() {
        let config = vapor_shared::config::VaporConfig {
            use_gitignore: false,
            use_vaporignore: true,
            pre_ignore_rules: vec!["tmp/".into(), " *.cache".into(), String::new()],
            post_ignore_rules: vec!["keep-out/".into()],
            ..vapor_shared::config::VaporConfig::default()
        };

        let options = EventPathFilterOptions::from_config(&config);

        assert!(!options.use_gitignore);
        assert!(options.use_vaporignore);
        assert_eq!(options.pre_user_rules, vec!["tmp/", "*.cache"]);
        assert_eq!(options.post_user_rules, vec!["keep-out/"]);
    }

    #[test]
    fn default_config_yields_the_compiled_default_rule_set() {
        let options =
            EventPathFilterOptions::from_config(&vapor_shared::config::VaporConfig::default());
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
