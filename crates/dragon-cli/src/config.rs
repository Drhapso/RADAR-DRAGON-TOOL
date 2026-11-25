//! dragon-cli::config - Configuración persistente local y de red LAN (`config/site_config.json`).
//!
//! Permite configurar de manera híbrida:
//! - `port`: Puerto TCP predeterminado (por defecto `8092`).
//! - `lan_mode`:
//!     - `"local_agent"` (default): En LAN, cada equipo cliente ejecuta su propio motor local
//!       (`127.0.0.1`) para indexar y buscar exclusivamente sus propios discos.
//!     - `"shared_server"`: En LAN, el servidor central indexa únicamente las carpetas declaradas
//!       en `shared_paths` y los clientes buscan, previsualizan y descargan directo desde el navegador.
//! - `shared_paths`: Carpetas locales autorizadas para indexación y acceso LAN en modo `"shared_server"`.
//! - `libreoffice_path`: Ruta opcional explícita a `soffice.com` o `soffice.exe`.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

pub const DEFAULT_UI_PORT: u16 = 8092;
pub const DEFAULT_SYNC_INTERVAL_MINUTES: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Cidr {
    network: u32,
    mask: u32,
}

impl Ipv4Cidr {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let (ip_str, prefix_str) = trimmed.split_once('/')?;
        let ip: Ipv4Addr = ip_str.trim().parse().ok()?;
        let prefix: u32 = prefix_str.trim().parse().ok()?;
        if prefix == 0 || prefix > 32 {
            return None;
        }
        let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
        let network = u32::from(ip) & mask;
        Some(Self { network, mask })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => (u32::from(v4) & self.mask) == self.network,
            IpAddr::V6(v6) => {
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    (u32::from(mapped) & self.mask) == self.network
                } else {
                    false
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LanMode {
    #[default]
    LocalAgent,
    SharedServer,
}

impl LanMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            LanMode::LocalAgent => "local_agent",
            LanMode::SharedServer => "shared_server",
        }
    }

    #[allow(dead_code)]
    pub fn parse_loose(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "local_agent" | "local-agent" | "agent" | "local" => Some(LanMode::LocalAgent),
            "shared_server" | "shared-server" | "shared" | "server" | "central" => {
                Some(LanMode::SharedServer)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteConfig {
    pub port: u16,
    pub lan_mode: LanMode,
    pub authorized_subnet: Option<String>,
    pub shared_paths: Vec<String>,
    pub libreoffice_path: Option<String>,
    pub sync_interval_minutes: u64,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_UI_PORT,
            lan_mode: LanMode::LocalAgent,
            authorized_subnet: None,
            shared_paths: Vec::new(),
            libreoffice_path: None,
            sync_interval_minutes: DEFAULT_SYNC_INTERVAL_MINUTES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedSiteConfig {
    pub config: SiteConfig,
    pub source_path: Option<PathBuf>,
    pub canonical_shared_roots: Vec<PathBuf>,
    pub authorized_cidr: Option<Ipv4Cidr>,
}

impl LoadedSiteConfig {
    /// Resuelve y carga `config/site_config.json` desde una ruta explícita, el directorio actual
    /// o junto al ejecutable `dragon.exe`. Si no existe, retorna los valores seguros por defecto.
    pub fn load(explicit_path: Option<&Path>) -> Self {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(p) = explicit_path {
            candidates.push(p.to_path_buf());
        } else {
            if let Ok(env_p) = std::env::var("DRAGON_CONFIG_PATH") {
                let trimmed = env_p.trim();
                if !trimmed.is_empty() {
                    candidates.push(PathBuf::from(trimmed));
                }
            }
            candidates.push(PathBuf::from("config").join("site_config.json"));
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    candidates.push(exe_dir.join("config").join("site_config.json"));
                    if let Some(repo_root) = exe_dir.parent().and_then(|p| p.parent()) {
                        candidates.push(repo_root.join("config").join("site_config.json"));
                    }
                }
            }
        }

        let mut loaded_cfg = SiteConfig::default();
        let mut source_path = None;

        for candidate in candidates {
            if candidate.is_file() {
                match std::fs::read_to_string(&candidate) {
                    Ok(raw) => {
                        let clean = raw.trim_start_matches('\u{feff}');
                        match serde_json::from_str::<SiteConfig>(clean) {
                            Ok(mut cfg) => {
                                if cfg.port == 0 {
                                    cfg.port = DEFAULT_UI_PORT;
                                }
                                if let Some(ref lo) = cfg.libreoffice_path {
                                    if lo.trim().is_empty() {
                                        cfg.libreoffice_path = None;
                                    }
                                }
                                if let Some(ref sub) = cfg.authorized_subnet {
                                    if sub.trim().is_empty() {
                                        cfg.authorized_subnet = None;
                                    }
                                }
                                loaded_cfg = cfg;
                                source_path = Some(candidate);
                                break;
                            }
                            Err(e) => {
                                log::warn!(
                                    "No se pudo parsear {} ({}); usando configuración por defecto",
                                    candidate.display(),
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("No se pudo leer {} ({})", candidate.display(), e);
                    }
                }
            }
        }

        let canonical_shared_roots = canonicalize_shared_roots(&loaded_cfg.shared_paths);
        let authorized_cidr = loaded_cfg
            .authorized_subnet
            .as_deref()
            .and_then(|raw| {
                let parsed = Ipv4Cidr::parse(raw);
                if parsed.is_none() {
                    log::warn!("Subred authorized_subnet inválida en site_config.json: '{}'", raw);
                }
                parsed
            });
        Self {
            config: loaded_cfg,
            source_path,
            canonical_shared_roots,
            authorized_cidr,
        }
    }

    /// Verifica si la IP remota está autorizada (siempre permite loopback; si `authorized_subnet`
    /// está definido, restringe accesos LAN exclusivamente a esa subred CIDR).
    pub fn is_peer_ip_authorized(&self, ip: IpAddr) -> bool {
        if ip.is_loopback() {
            return true;
        }
        match self.authorized_cidr {
            Some(cidr) => cidr.contains(ip),
            None => true,
        }
    }

    pub fn with_lan_mode_override(mut self, mode: Option<LanMode>) -> Self {
        if let Some(m) = mode {
            self.config.lan_mode = m;
        }
        self
    }

    pub fn is_shared_server(&self) -> bool {
        self.config.lan_mode == LanMode::SharedServer
    }

    /// Verifica de forma estricta (canonicalizando la ruta) si un archivo está contenido
    /// dentro de alguna de las carpetas autorizadas en `shared_paths`.
    pub fn is_path_allowed_for_remote(&self, raw_path: &str) -> bool {
        if !self.is_shared_server() || self.canonical_shared_roots.is_empty() {
            return false;
        }
        is_path_within_roots(Path::new(raw_path), &self.canonical_shared_roots)
    }

    /// Verifica por prefijo normalizado (rápido, sin tocar disco por cada hit) si una ruta
    /// indexada pertenece a las carpetas autorizadas en `shared_paths`.
    pub fn is_indexed_path_in_shared_roots(&self, raw_path: &str) -> bool {
        if self.canonical_shared_roots.is_empty() {
            return false;
        }
        let norm = normalize_for_prefix_compare(Path::new(raw_path));
        self.canonical_shared_roots.iter().any(|root| {
            let root_norm = normalize_for_prefix_compare(root);
            path_has_prefix(&norm, &root_norm)
        })
    }
}

pub fn canonicalize_shared_roots(paths: &[String]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for raw in paths {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let p = PathBuf::from(trimmed);
        if p.is_dir() {
            let canon = std::fs::canonicalize(&p).unwrap_or(p);
            let cleaned = strip_windows_verbatim_prefix(&canon);
            if !roots.iter().any(|r: &PathBuf| {
                normalize_for_prefix_compare(r) == normalize_for_prefix_compare(&cleaned)
            }) {
                roots.push(cleaned);
            }
        } else {
            log::warn!(
                "Carpeta compartida configurada en site_config.json no existe o no es directorio: {}",
                trimmed
            );
        }
    }
    roots
}

pub fn strip_windows_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{}", rest));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

fn normalize_for_prefix_compare(p: &Path) -> String {
    let cleaned = strip_windows_verbatim_prefix(p);
    let mut s = cleaned.to_string_lossy().replace('/', "\\");
    if cfg!(windows) {
        s = s.to_ascii_lowercase();
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

fn path_has_prefix(candidate: &str, root: &str) -> bool {
    if candidate == root {
        return true;
    }
    if root.ends_with('\\') {
        candidate.starts_with(root)
    } else {
        candidate.starts_with(root)
            && candidate.as_bytes().get(root.len()) == Some(&b'\\')
    }
}

pub fn is_path_within_roots(candidate: &Path, roots: &[PathBuf]) -> bool {
    let Ok(canon) = std::fs::canonicalize(candidate) else {
        return false;
    };
    let cand_norm = normalize_for_prefix_compare(&canon);
    roots.iter().any(|root| {
        let root_norm = normalize_for_prefix_compare(root);
        path_has_prefix(&cand_norm, &root_norm)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_port_8092_and_local_agent() {
        let cfg = SiteConfig::default();
        assert_eq!(cfg.port, 8092);
        assert_eq!(cfg.lan_mode, LanMode::LocalAgent);
        assert!(cfg.shared_paths.is_empty());
    }

    #[test]
    fn prefix_boundary_prevents_sibling_directory_escape() {
        assert!(path_has_prefix(r"d:\compartido\sub\doc.pdf", r"d:\compartido"));
        assert!(path_has_prefix(r"d:\compartido", r"d:\compartido"));
        assert!(!path_has_prefix(r"d:\compartido_privado\doc.pdf", r"d:\compartido"));
        assert!(path_has_prefix(r"d:\doc.pdf", r"d:\"));
    }

    #[test]
    fn canonical_containment_blocks_traversal() {
        let base = std::env::temp_dir().join(format!("dragon_cfg_test_{}", std::process::id()));
        let allowed = base.join("shared");
        let secret = base.join("secret");
        let _ = std::fs::create_dir_all(&allowed);
        let _ = std::fs::create_dir_all(&secret);

        let ok_file = allowed.join("ok.txt");
        let bad_file = secret.join("no.txt");
        let _ = std::fs::write(&ok_file, b"ok");
        let _ = std::fs::write(&bad_file, b"secret");

        let roots = canonicalize_shared_roots(&[allowed.to_string_lossy().to_string()]);
        assert!(is_path_within_roots(&ok_file, &roots));
        assert!(!is_path_within_roots(&bad_file, &roots));

        let traversal = allowed.join("..").join("secret").join("no.txt");
        assert!(!is_path_within_roots(&traversal, &roots));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parse_lan_mode_variants() {
        assert_eq!(LanMode::parse_loose("local_agent"), Some(LanMode::LocalAgent));
        assert_eq!(LanMode::parse_loose("shared_server"), Some(LanMode::SharedServer));
        assert_eq!(LanMode::parse_loose("shared"), Some(LanMode::SharedServer));
        assert_eq!(LanMode::parse_loose("invalido"), None);
    }

    #[test]
    fn cidr_subnet_authorization_matches_eva_pdf_lan() {
        let cidr = Ipv4Cidr::parse("172.23.174.0/24").expect("CIDR válido");
        assert!(cidr.contains(IpAddr::V4(Ipv4Addr::new(172, 23, 174, 11))));
        assert!(cidr.contains(IpAddr::V4(Ipv4Addr::new(172, 23, 174, 103))));
        assert!(!cidr.contains(IpAddr::V4(Ipv4Addr::new(172, 23, 175, 10))));
        assert!(!cidr.contains(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5))));
    }
}
