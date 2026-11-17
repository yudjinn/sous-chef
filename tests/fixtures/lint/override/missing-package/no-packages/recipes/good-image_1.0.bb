# An image: PACKAGES is empty, so OE-core does not run pkgvarcheck.
inherit core-image
require good-image.inc
RDEPENDS = "bash"
