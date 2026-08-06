//! Named groups of host globs for [`Settings::passthrough_presets`], covering
//! clients that break under MITM because they ship their own CA bundle, use
//! mTLS, or pin certificates: cloud CLIs, container registries, and the
//! handful of mobile apps/OS services known to hard-fail rather than just
//! showing a broken request.
//!
//! [`Settings::passthrough_presets`]: crate::settings::Settings::passthrough_presets

/// One named group of passthrough host globs.
pub struct Preset {
    /// Stable identifier persisted in `settings.json` (e.g. `"cloud-cli"`).
    pub name: &'static str,
    /// Short human-readable name for the UI.
    pub label: &'static str,
    /// One-sentence explanation of why these hosts are excluded, shown as UI
    /// helper text.
    pub description: &'static str,
    /// Host globs belonging to this preset.
    pub hosts: &'static [&'static str],
}

/// All built-in presets, in the order they're shown in the UI and iterated
/// by [`expand`].
pub const PRESETS: &[Preset] = &[
    // Loopback (`localhost`, `127.0.0.1`, `::1`, `*.local`) is deliberately
    // NOT in this preset. Intercepting a developer's own local server is a
    // primary use case for hamsy -- excluding it here would silently
    // blind-tunnel that traffic instead of decrypting it. Loopback only
    // needs to be excluded from the *OS-level* system proxy (see
    // `SYSTEM_PROXY_BYPASS` in hamsy-cli) so hamsy doesn't try to proxy
    // itself; it should never be excluded from MITM.
    Preset {
        name: "core",
        label: "Metadata & cluster",
        description: "Cloud instance-metadata endpoints and in-cluster Kubernetes service names -- clients that use mTLS or a cluster-supplied CA, which interception breaks outright.",
        hosts: &[
            "169.254.169.254",
            "metadata.google.internal",
            "*.cluster.local",
            "*.svc",
            "kubernetes.default.svc",
        ],
    },
    Preset {
        name: "cloud-cli",
        label: "Cloud CLIs & registries",
        description: "gcloud, kubectl, aws, and container registries — these ship their own CA bundles or use mTLS client certificates, so interception breaks them outright.",
        hosts: &[
            "googleapis.com",
            "*.googleapis.com",
            "*.mtls.googleapis.com",
            "accounts.google.com",
            "oauth2.googleapis.com",
            "sts.googleapis.com",
            "*.googleusercontent.com",
            "*.googlesource.com",
            "dl.google.com",
            "gcr.io",
            "*.gcr.io",
            "*.pkg.dev",
            "*.amazonaws.com",
            "*.eks.amazonaws.com",
            "login.microsoftonline.com",
            "*.azmk8s.io",
            "*.gke.goog",
            "registry-1.docker.io",
            "auth.docker.io",
        ],
    },
    Preset {
        name: "meta",
        label: "Meta apps",
        description: "Facebook, Instagram, WhatsApp, Messenger and Threads pin their certificates; intercepting them makes the apps fail to load rather than showing traffic.",
        hosts: &[
            "facebook.com",
            "*.facebook.com",
            "*.facebook.net",
            "*.fbcdn.net",
            "*.fbsbx.com",
            "instagram.com",
            "*.instagram.com",
            "*.cdninstagram.com",
            "whatsapp.com",
            "*.whatsapp.com",
            "*.whatsapp.net",
            "*.messenger.com",
            "*.threads.net",
            "*.meta.com",
            "*.oculus.com",
        ],
    },
    Preset {
        name: "mobile-os",
        label: "Mobile OS services",
        description: "Apple and Google device services — push, App Store, iCloud. Intercepting these makes a proxied phone behave erratically.",
        hosts: &[
            "*.apple.com",
            "*.icloud.com",
            "*.mzstatic.com",
            "*.cdn-apple.com",
            "*.push.apple.com",
            "*.aaplimg.com",
            "*.apple-cloudkit.com",
            "android.clients.google.com",
            "*.gvt1.com",
        ],
    },
];

/// Expands preset `names` into the union of their host globs, in [`PRESETS`]
/// order with duplicates removed. Names that don't match any preset are
/// silently ignored -- forward-compat with a `settings.json` written by a
/// newer build that shipped a preset this version doesn't know about.
pub fn expand(names: &[String]) -> Vec<String> {
    let mut hosts = Vec::new();
    for preset in PRESETS {
        if !names.iter().any(|n| n == preset.name) {
            continue;
        }
        for host in preset.hosts {
            let host = host.to_string();
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

/// The preset names enabled by default -- all of them.
pub fn default_enabled() -> Vec<String> {
    PRESETS.iter().map(|p| p.name.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_all_presets_is_non_empty_and_covers_known_hosts() {
        let hosts = expand(&default_enabled());
        assert!(!hosts.is_empty());
        assert!(hosts.contains(&"*.googleapis.com".to_string()));
        assert!(hosts.contains(&"*.whatsapp.net".to_string()));
    }

    #[test]
    fn expand_ignores_unknown_names() {
        let hosts = expand(&["not-a-real-preset".to_string()]);
        assert!(hosts.is_empty());
    }

    #[test]
    fn expand_empty_slice_is_empty() {
        assert!(expand(&[]).is_empty());
    }
}
