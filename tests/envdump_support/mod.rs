//! Shared by `tests/envdump.rs` and `tests/var_authoritative.rs`: the recorded
//! `bitbake -e curl` fixture and a small workspace laid out like the live environment it
//! was recorded in, so that the paths in the dump point into it.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::config::Config;

/// The trimmed recording, with `{{LIVE}}` placeholders.
pub fn fixture_text() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/outputs/envdump/bitbake-e-curl.out");
    fs::read_to_string(p).unwrap()
}

/// The recording with `{{LIVE}}` replaced by `live`.
pub fn fixture_at(live: &Path) -> String {
    fixture_text().replace("{{LIVE}}", &live.display().to_string())
}

/// Lines of the fixture that mark a cut (`# [fixture] N lines cut`).
pub fn cut_markers(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("# [fixture] "))
        .count()
}

/// A workspace under `<tmp>/live` laid out like the recording's environment:
/// `yocto-kiss/build` (the build directory), the layers `openembedded-core/meta` (collection
/// `core`) and `meta-kiss`, a `curl_8.19.0.bb` and a `curl_%.bbappend`. Returns the live
/// root (canonical) and a configuration that runs the fake BitBake through
/// `tests/fixtures/fakebb/init-with-env`.
pub struct LiveWs {
    pub root: PathBuf,
    pub config: Config,
}

impl LiveWs {
    pub fn new(tmp: &Path) -> LiveWs {
        let root = tmp.join("live");
        let kiss = root.join("yocto-kiss");
        let build = kiss.join("build");
        write(
            &build.join("conf/bblayers.conf"),
            "BBPATH = \"${TOPDIR}\"\nBBFILES ?= \"\"\nBBLAYERS ?= \" \\\n  ${TOPDIR}/../openembedded-core/meta \\\n  ${TOPDIR}/../meta-kiss \\\n  \"\n",
        );
        write(
            &build.join("conf/local.conf"),
            "MACHINE ?= \"dogbonedark\"\nDISTRO ?= \"kiss\"\n",
        );
        let meta = kiss.join("openembedded-core/meta");
        write(
            &meta.join("conf/layer.conf"),
            "BBPATH .= \":${LAYERDIR}\"\nBBFILES += \"${LAYERDIR}/recipes-*/*/*.bb ${LAYERDIR}/recipes-*/*/*.bbappend\"\nBBFILE_COLLECTIONS += \"core\"\nBBFILE_PATTERN_core = \"^${LAYERDIR}/\"\nBBFILE_PRIORITY_core = \"5\"\nLAYERSERIES_CORENAMES = \"wrynose\"\nLAYERSERIES_COMPAT_core = \"wrynose\"\n",
        );
        write(
            &meta.join("conf/bitbake.conf"),
            "TMPDIR ?= \"${TOPDIR}/tmp\"\nSRC_URI = \"\"\nDEPENDS = \"\"\n",
        );
        write(
            &meta.join("classes-global/base.bbclass"),
            "DEPENDS:prepend = \"${BASEDEPENDS} \"\n",
        );
        write(
            &meta.join("recipes-support/curl/curl_8.19.0.bb"),
            "SUMMARY = \"Command line tool and library for client-side URL transfers\"\nLICENSE = \"curl\"\nSRC_URI = \"https://curl.se/download/${BP}.tar.xz\"\nPACKAGECONFIG ??= \"openssl zlib\"\n",
        );
        let mk = kiss.join("meta-kiss");
        write(
            &mk.join("conf/layer.conf"),
            "BBPATH .= \":${LAYERDIR}\"\nBBFILES += \"${LAYERDIR}/recipes-*/*/*.bb ${LAYERDIR}/recipes-*/*/*.bbappend\"\nBBFILE_COLLECTIONS += \"kiss\"\nBBFILE_PATTERN_kiss = \"^${LAYERDIR}/\"\nBBFILE_PRIORITY_kiss = \"6\"\nLAYERSERIES_COMPAT_kiss = \"wrynose\"\n",
        );
        write(
            &mk.join("recipes-support/curl/curl_%.bbappend"),
            "EXTRA_OECONF:append = \" --enable-kiss\"\n",
        );
        let root = fs::canonicalize(&root).unwrap();
        let build = root.join("yocto-kiss/build");
        let mut config = Config {
            build_dir: Some(build),
            work_dir: root.join("yocto-kiss"),
            ..Config::default()
        };
        config.bitbake.enabled = true;
        config.init_script =
            Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fakebb/init-with-env"));
        LiveWs { root, config }
    }

    pub fn build(&self) -> PathBuf {
        self.root.join("yocto-kiss/build")
    }

    /// The fixture with `{{LIVE}}` replaced by this workspace's root, written next to the
    /// build directory; returns its path.
    pub fn write_dump(&self) -> PathBuf {
        let p = self.root.join("bitbake-e-curl.out");
        fs::write(&p, fixture_at(&self.root)).unwrap();
        p
    }

    /// Settings for the next fake bitbake run (`FAKEBB_*`, see `tests/fixtures/fakebb`).
    pub fn fake(&self, vars: &[(&str, &str)]) {
        let text: String = vars
            .iter()
            .map(|(k, v)| format!("export {k}='{v}'\n"))
            .collect();
        fs::write(self.build().join("fakebb.env"), text).unwrap();
    }
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
