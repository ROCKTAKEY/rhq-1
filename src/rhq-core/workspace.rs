use std::borrow::Cow;
use std::fmt::Arguments;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use glob::Pattern;
use walkdir::{DirEntry, WalkDir, WalkDirIterator};

use cache::Cache;
use config::Config;
use repository::{Remote, Repository};
use query::Query;
use vcs;


#[derive(Default)]
pub struct Printer {
    verbose: bool,
}

impl Printer {
    pub fn print(&self, args: Arguments) {
        if self.verbose {
            let stdout = io::stdout();
            let _ = stdout.lock().write_fmt(args);
        }
    }
}


pub struct Workspace<'a> {
    cache: Cache,
    config: Config,
    root: Option<&'a Path>,
    printer: Printer,
}

impl<'a> Workspace<'a> {
    pub fn new() -> ::Result<Self> {
        let config = Config::new(None)?;
        let cache = Cache::new(None)?;
        Ok(Workspace {
            cache: cache,
            config: config,
            root: None,
            printer: Printer::default(),
        })
    }

    pub fn root_dir(mut self, root: Option<&'a Path>) -> Self {
        self.root = root;
        self
    }

    pub fn verbose_output(mut self, verbose: bool) -> Self {
        self.printer.verbose = verbose;
        self
    }

    pub fn print(&self, args: Arguments) {
        self.printer.print(args)
    }

    /// Returns a list of managed repositories.
    /// Note that this method returns None if cache has not created yet.
    pub fn repositories(&self) -> Option<&[Repository]> {
        self.cache
            .get_opt()
            .map(|cache| cache.repositories.as_slice())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn import_repositories<P: AsRef<Path>>(&mut self, root: P, depth: Option<usize>) -> ::Result<()> {
        for path in collect_repositories(root, depth, self.config.exclude_patterns()) {
            if let Some(repo) = self.new_repository_from_path(&path)? {
                self.add_repository(repo);
            }
        }
        Ok(())
    }


    pub fn add_repository(&mut self, repo: Repository) {
        let ref mut repos = self.cache.get_mut().repositories;
        if let Some(r) = repos.iter_mut().find(|r| r.is_same_local(&repo)) {
            self.printer.print(format_args!(
                "Overwrite existed entry: {}\n",
                repo.path_string()
            ));
            *r = repo;
            return;
        }

        self.printer
            .print(format_args!("Add new entry: {}\n", repo.path_string()));
        repos.push(repo);
    }

    pub fn drop_invalid_repositories(&mut self) {
        let mut new_repo = Vec::new();
        for repo in &self.cache.get_mut().repositories {
            let repo = match repo.clone().refresh() {
                Some(r) => r,
                None => continue,
            };
            if self.config
                .exclude_patterns()
                .into_iter()
                .all(|ex| !ex.matches(&repo.path_string()))
            {
                new_repo.push(repo.clone());
            } else {
                self.printer
                    .print(format_args!("Dropped: {}\n", repo.path_string()));
            }
        }
        self.cache.get_mut().repositories = new_repo;
    }

    pub fn sort_repositories(&mut self) {
        self.cache
            .get_mut()
            .repositories
            .sort_by(|a, b| a.name().cmp(b.name()));
    }


    /// Save current state of workspace to cache file.
    pub fn save_cache(&mut self) -> ::Result<()> {
        self.cache.dump()?;
        Ok(())
    }

    pub fn resolve_query(&self, query: &Query) -> ::Result<PathBuf> {
        let root: Cow<Path> = self.root
            .map(Into::into)
            .or_else(|| self.config.root_dir().map(Into::into))
            .ok_or("Unknown root directory")?;
        let host = query.host().unwrap_or("github.com");
        let path = root.join(host).join(&*query.path());
        Ok(path)
    }

    pub fn for_each_repo<F: Fn(&Repository) -> ::Result<()>>(&self, f: F) -> ::Result<()> {
        let repos = self.repositories()
            .ok_or("The cache has not initialized yet")?;
        for repo in repos {
            f(&repo)?;
        }
        Ok(())
    }

    pub fn new_repository_from_path(&self, path: &Path) -> ::Result<Option<Repository>> {
        let vcs = match vcs::detect_from_path(&path) {
            Some(vcs) => vcs,
            None => return Ok(None),
        };
        let remote = match vcs.get_remote_url(&path)? {
            Some(remote) => remote,
            None => return Ok(None),
        };
        Repository::new(path, vcs, Remote::new(remote)).map(Some)
    }
}


fn collect_repositories<P>(root: P, depth: Option<usize>, excludes: Vec<Pattern>) -> Vec<PathBuf>
where
    P: AsRef<Path>,
{
    let filter = {
        let root = root.as_ref();
        move |entry: &DirEntry| {
            if entry.path() == root {
                return true;
            }
            !entry
                .path()
                .parent()
                .map(|path| vcs::detect_from_path(&path).is_some())
                .unwrap_or(false)
                && entry
                    .path()
                    .canonicalize()
                    .ok()
                    .map(|path| {
                        let path = path.to_str().unwrap().trim_left_matches(r"\\?\");
                        excludes.iter().all(|ex| !ex.matches(path))
                    })
                    .unwrap_or(false)
        }
    };

    let mut walkdir = WalkDir::new(root.as_ref()).follow_links(true);
    if let Some(depth) = depth {
        walkdir = walkdir.max_depth(depth);
    }
    walkdir
        .into_iter()
        .filter_entry(filter)
        .filter_map(Result::ok)
        .filter(|entry| vcs::detect_from_path(entry.path()).is_some())
        .map(|entry| entry.path().into())
        .collect()
}
