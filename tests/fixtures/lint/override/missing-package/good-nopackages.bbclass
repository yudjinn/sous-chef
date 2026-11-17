# OE-core image.bbclass:31-35: the class empties PACKAGES, so pkgvarcheck never runs for
# the recipes it is meant for.
LICENSE ?= "MIT"
PACKAGES = ""
DEPENDS += "depmodwrapper-cross"
RDEPENDS += "${PACKAGE_INSTALL} ${LINGUAS_INSTALL}"
RRECOMMENDS += "${PACKAGE_INSTALL_ATTEMPTONLY}"
