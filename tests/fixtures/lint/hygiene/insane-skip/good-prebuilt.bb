SUMMARY = "Prebuilt binaries"
LICENSE = "CLOSED"
inherit bin_package
INSANE_SKIP:${PN} += "ldflags"
