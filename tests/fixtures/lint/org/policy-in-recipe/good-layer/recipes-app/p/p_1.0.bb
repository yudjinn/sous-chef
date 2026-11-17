SUMMARY = "x"
LICENSE = "MIT"
REQUIRED_DISTRO_FEATURES = "x11"
EXTRA_OECONF += "${@bb.utils.contains('DISTRO_FEATURES', 'x11', '--x', '', d)}"
