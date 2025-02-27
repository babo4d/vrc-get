use super::*;
use std::collections::HashMap;
use std::future::Future;

#[derive(Debug)]
pub(super) struct RepoHolder {
    cached_repos_new: HashMap<PathBuf, LocalCachedRepository>,
}

impl RepoHolder {
    pub(crate) fn new() -> Self {
        RepoHolder {
            cached_repos_new: HashMap::new(),
        }
    }
}

// new system
impl RepoHolder {
    pub(crate) async fn load_repos(&mut self, http: Option<&Client>, sources: Vec<RepoSource>) -> io::Result<()> {
        fn file_path(source: &RepoSource) -> &Path {
            match source {
                RepoSource::PreDefined(_, path) => path,
                RepoSource::UserRepo(user) => &user.local_path,
            }
        }

        let repos = try_join_all(sources.iter().map(|src| async {
            Self::load_repo_from_source(http, src).await
                .map(|v| v.map(|v| (v, file_path(src))))
        }))
        .await?;

        for (repo, path) in repos.into_iter().flatten() {
            self.cached_repos_new.insert(path.to_owned(), repo);
        }

        Ok(())
    }

    async fn load_repo_from_source(
        client: Option<&Client>,
        source: &RepoSource,
    ) -> io::Result<Option<LocalCachedRepository>> {
        match source {
            RepoSource::PreDefined(source, path) => {
                RepoHolder::load_remote_repo(
                    client,
                    None,
                    &path, 
                    source.url,
                ).await.map(Some)
            }
            RepoSource::UserRepo(user_repo) => {
                if let Some(url) = &user_repo.url {
                    RepoHolder::load_remote_repo(
                        client,
                        Some(&user_repo.headers),
                        &user_repo.local_path,
                        &url,
                    )
                    .await.map(Some)
                } else {
                    RepoHolder::load_local_repo(client, &user_repo.local_path).await.map(Some)
                }
            }
        }
    }

    async fn load_remote_repo(
        client: Option<&Client>,
        headers: Option<&IndexMap<String, String>>,
        path: &Path,
        remote_url: &str,
    ) -> io::Result<LocalCachedRepository> {
        Self::load_repo(path, client, || async {
            // if local repository not found: try downloading remote one
            let Some(client) = client else {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "offline mode"))
            };
            let (remote_repo, etag) = download_remote_repository(&client, remote_url, headers, None)
                .await?
                .expect("logic failure: no etag");

            let mut local_cache = LocalCachedRepository::new(remote_repo, headers.map(Clone::clone).unwrap_or_default());

            if let Some(etag) = etag {
                local_cache
                    .vrc_get
                    .get_or_insert_with(Default::default)
                    .etag = etag;
            }

            match write_repo(path, &local_cache).await {
                Ok(_) => {}
                Err(e) => {
                    log::error!("writing local repo '{}': {}", path.display(), e);
                }
            }

            Ok(local_cache)
        })
        .await
    }

    async fn load_local_repo(
        client: Option<&Client>,
        path: &Path,
    ) -> io::Result<LocalCachedRepository> {
        Self::load_repo(path, client, || async { unreachable!() }).await
    }

    async fn load_repo<F, T>(
        path: &Path,
        http: Option<&Client>,
        if_not_found: F,
    ) -> io::Result<LocalCachedRepository>
    where
        F: FnOnce() -> T,
        T: Future<Output = io::Result<LocalCachedRepository>>,
    {
        let Some(json_file) = try_open_file(path).await? else {
            return Ok(if_not_found().await?);
        };

        let mut loaded = match serde_json::from_slice(&read_to_vec(json_file).await?) {
            Ok(loaded) => loaded,
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("loading {}: {}", path.display(), e),
                ))
            }
        };
        if let Some(http) = http {
            update_from_remote(http, path.into(), &mut loaded).await;
        }
        return Ok(loaded);
    }

    pub(crate) fn get_repos(&self) -> Vec<&LocalCachedRepository> {
        self.cached_repos_new.values().collect()
    }

    pub(crate) fn get_repo_with_path(&self) -> impl Iterator<Item = (&'_ PathBuf, &'_ LocalCachedRepository)> {
        self.cached_repos_new.iter()
    }

    pub(crate) fn get_repo(&self, path: &Path) -> Option<&LocalCachedRepository> {
        self.cached_repos_new.get(path)
    }

    pub(crate) fn remove_repo(&mut self, path: &Path) {
        self.cached_repos_new.remove(path);
    }
}
