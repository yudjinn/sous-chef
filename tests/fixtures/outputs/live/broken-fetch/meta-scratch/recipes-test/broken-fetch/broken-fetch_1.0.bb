SUMMARY = "T9.2 scratch recipe: do_fetch fails on purpose (reserved .invalid domain)"
LICENSE = "CLOSED"

SRC_URI = "https://sous-chef-verify.invalid/broken-fetch-${PV}.tar.gz"
SRC_URI[sha256sum] = "0000000000000000000000000000000000000000000000000000000000000000"

# Never built: only do_fetch is run.
EXCLUDE_FROM_WORLD = "1"
