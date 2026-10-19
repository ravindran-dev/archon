use std::cell::RefCell;
use std::collections::HashMap;

use archon_types::{PackageMeta, PackageProvider, ProviderError};

use crate::model::{AurPackage, RpcResponse};

/// The AUR rejects overly long query strings; batch info requests well
/// below that limit.
const INFO_CHUNK: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum AurError {
    #[error("AUR RPC request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("AUR RPC returned an error: {0}")]
    Rpc(String),
    #[error("AUR package {name} has malformed metadata: {detail}")]
    BadMetadata { name: String, detail: String },
}

/// Blocking AUR RPC client with a per-instance response cache.
///
/// Blocking is a deliberate choice for Phase 1: the resolver is a
/// synchronous graph walk, `info` is batched (one round-trip for N names),
/// and it keeps async out of every crate. If profiling ever shows the RPC
/// as a bottleneck we can revisit.
pub struct AurClient {
    http: reqwest::blocking::Client,
    base_url: String,
    /// name -> Some(meta) if it exists on the AUR, None if a previous query
    /// established it doesn't (negative caching matters: the resolver probes
    /// the AUR for every name the official repos couldn't satisfy).
    cache: RefCell<HashMap<String, Option<PackageMeta>>>,
    /// name -> PackageBase, needed to derive clone URLs for split packages.
    bases: RefCell<HashMap<String, String>>,
}

impl AurClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, AurError> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent(concat!("archon/", env!("CARGO_PKG_VERSION")))
                .build()?,
            base_url: base_url.into(),
            cache: RefCell::new(HashMap::new()),
            bases: RefCell::new(HashMap::new()),
        })
    }

    /// Batch metadata lookup via `/rpc/v5/info`. Consults the cache first
    /// and only puts cache misses on the wire. Names the AUR doesn't know
    /// are negative-cached and absent from the result.
    pub fn info(&self, names: &[String]) -> Result<Vec<PackageMeta>, AurError> {
        let misses: Vec<&String> = {
            let cache = self.cache.borrow();
            names.iter().filter(|n| !cache.contains_key(*n)).collect()
        };

        for chunk in misses.chunks(INFO_CHUNK) {
            let params: Vec<(&str, &str)> = chunk.iter().map(|n| ("arg[]", n.as_str())).collect();
            let resp: RpcResponse = self
                .http
                .get(format!("{}/rpc/v5/info", self.base_url))
                .query(&params)
                .send()?
                .error_for_status()?
                .json()?;
            if resp.kind == "error" {
                return Err(AurError::Rpc(
                    resp.error.unwrap_or_else(|| "unknown error".into()),
                ));
            }
            let mut cache = self.cache.borrow_mut();
            let mut bases = self.bases.borrow_mut();
            // Negative-cache everything we asked for...
            for name in chunk {
                cache.entry((*name).clone()).or_insert(None);
            }
            // ...then overwrite with what actually came back.
            for pkg in resp.results {
                bases.insert(pkg.name.clone(), pkg.package_base.clone());
                let meta = pkg.into_meta()?;
                cache.insert(meta.name.clone(), Some(meta));
            }
        }

        let cache = self.cache.borrow();
        Ok(names
            .iter()
            .filter_map(|n| cache.get(n).and_then(|m| m.clone()))
            .collect())
    }

    /// `GET /rpc/v5/search/{dep_name}?by=provides`. Search results carry
    /// only partial metadata, so this returns names for a follow-up
    /// [`AurClient::info`].
    pub fn search_by_provides(&self, dep_name: &str) -> Result<Vec<AurPackage>, AurError> {
        let resp: RpcResponse = self
            .http
            .get(format!("{}/rpc/v5/search/{dep_name}", self.base_url))
            .query(&[("by", "provides")])
            .send()?
            .error_for_status()?
            .json()?;
        if resp.kind == "error" {
            return Err(AurError::Rpc(
                resp.error.unwrap_or_else(|| "unknown error".into()),
            ));
        }
        Ok(resp.results)
    }

    /// `GET /rpc/v5/search/{term}?by=name` — substring match against
    /// package names only (not descriptions), for `archon search` and "did
    /// you mean" suggestions when an exact name isn't found. Results carry
    /// partial metadata only, same caveat as
    /// [`AurClient::search_by_provides`].
    pub fn search_by_name(&self, term: &str) -> Result<Vec<AurPackage>, AurError> {
        let resp: RpcResponse = self
            .http
            .get(format!("{}/rpc/v5/search/{term}", self.base_url))
            .query(&[("by", "name")])
            .send()?
            .error_for_status()?
            .json()?;
        if resp.kind == "error" {
            return Err(AurError::Rpc(
                resp.error.unwrap_or_else(|| "unknown error".into()),
            ));
        }
        Ok(resp.results)
    }

    /// AUR git clone URL for a package previously seen by this client.
    /// Split packages clone by their PackageBase, not the package name.
    pub fn clone_url_for(&self, pkg_name: &str) -> Option<String> {
        self.bases
            .borrow()
            .get(pkg_name)
            .map(|base| format!("{}/{}.git", self.base_url, base))
    }

    pub fn package_base(&self, pkg_name: &str) -> Option<String> {
        self.bases.borrow().get(pkg_name).cloned()
    }
}

impl PackageProvider for AurClient {
    fn label(&self) -> &str {
        "aur"
    }

    fn get(&self, names: &[String]) -> Result<Vec<PackageMeta>, ProviderError> {
        self.info(names)
            .map_err(|e| ProviderError::Backend(Box::new(e)))
    }

    fn find_providers(&self, dep_name: &str) -> Result<Vec<PackageMeta>, ProviderError> {
        let wrap = |e: AurError| ProviderError::Backend(Box::new(e));
        let hits = self.search_by_provides(dep_name).map_err(wrap)?;
        let names: Vec<String> = hits.into_iter().map(|p| p.name).collect();
        if names.is_empty() {
            return Ok(vec![]);
        }
        // Search matches loosely; re-fetch full metadata and keep only
        // packages that literally provide `dep_name`.
        let metas = self.info(&names).map_err(wrap)?;
        Ok(metas
            .into_iter()
            .filter(|m| m.provides.iter().any(|p| p.name == dep_name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal canned-response HTTP server: routes are (path-substring,
    /// body) pairs; each accepted connection gets the first matching route.
    fn serve(routes: Vec<(&'static str, String)>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                hits2.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                // Drain headers.
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let body = routes
                    .iter()
                    .find(|(needle, _)| request_line.contains(needle))
                    .map(|(_, body)| body.clone())
                    .unwrap_or_else(|| r#"{"type":"error","error":"no route"}"#.into());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn rpc_result(pkgs: &str) -> String {
        format!(r#"{{"type":"multiinfo","resultcount":1,"results":[{pkgs}],"version":5}}"#)
    }

    const PARU: &str = r#"{"Name":"paru","Version":"2.0.4-1","PackageBase":"paru",
        "Description":"AUR helper","Depends":["git"],"MakeDepends":["cargo"]}"#;

    #[test]
    fn info_fetches_and_caches() {
        let (url, hits) = serve(vec![("/rpc/v5/info", rpc_result(PARU))]);
        let client = AurClient::new(&url).unwrap();

        let metas = client.info(&["paru".into()]).unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "paru");
        assert_eq!(client.package_base("paru").as_deref(), Some("paru"));
        assert_eq!(
            client.clone_url_for("paru"),
            Some(format!("{url}/paru.git"))
        );

        // Second lookup must be served from cache: no new connection.
        let before = hits.load(Ordering::SeqCst);
        let again = client.info(&["paru".into()]).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), before);
    }

    #[test]
    fn unknown_packages_are_negative_cached() {
        let (url, hits) = serve(vec![(
            "/rpc/v5/info",
            r#"{"type":"multiinfo","resultcount":0,"results":[],"version":5}"#.into(),
        )]);
        let client = AurClient::new(&url).unwrap();
        assert!(client.info(&["nope".into()]).unwrap().is_empty());
        let before = hits.load(Ordering::SeqCst);
        assert!(client.info(&["nope".into()]).unwrap().is_empty());
        assert_eq!(
            hits.load(Ordering::SeqCst),
            before,
            "negative result not cached"
        );
    }

    #[test]
    fn rpc_error_body_becomes_error() {
        let (url, _) = serve(vec![(
            "/rpc/v5/info",
            r#"{"type":"error","results":[],"error":"Incorrect request type specified."}"#.into(),
        )]);
        let client = AurClient::new(&url).unwrap();
        let err = client.info(&["x".into()]).unwrap_err();
        assert!(matches!(err, AurError::Rpc(_)), "got: {err}");
    }

    #[test]
    fn find_providers_refetches_and_filters() {
        let search = r#"{"type":"search","resultcount":1,"results":[
            {"Name":"jdk-openjdk-git","Version":"22-1","PackageBase":"jdk-openjdk-git"}
        ],"version":5}"#;
        let full = r#"{"Name":"jdk-openjdk-git","Version":"22-1","PackageBase":"jdk-openjdk-git",
            "Provides":["jdk=22"],"Depends":[]}"#;
        let (url, _) = serve(vec![
            ("/rpc/v5/search/jdk", search.to_string()),
            ("/rpc/v5/info", rpc_result(full)),
        ]);
        let client = AurClient::new(&url).unwrap();
        let providers = client.find_providers("jdk").unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name, "jdk-openjdk-git");
    }

    #[test]
    fn search_by_name_returns_partial_hits() {
        let search = r#"{"type":"search","resultcount":2,"results":[
            {"Name":"cuttlefish-base-git","Version":"1:r6181-1","PackageBase":"cuttlefish-base-git"},
            {"Name":"cuttlefish-user-git","Version":"1:r6181-1","PackageBase":"cuttlefish-user-git"}
        ],"version":5}"#;
        let (url, _) = serve(vec![("/rpc/v5/search/cuttlefish", search.to_string())]);
        let client = AurClient::new(&url).unwrap();
        let hits = client.search_by_name("cuttlefish").unwrap();
        let names: Vec<_> = hits.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["cuttlefish-base-git", "cuttlefish-user-git"]);
    }
}
