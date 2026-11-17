# meta-kiss:recipes-core/images/kiss-image.bb (yocto-kiss cc621c5d), alone in images/.
# Build a simple, minimal root filesystem.
#
# This recipe is a simplified form of core-image-minimal.

SUMMARY = "A simple, minimal image"

IMAGE_INSTALL = "packagegroup-core-boot dropbear sl"

IMAGE_LINGUAS = " "

inherit core-image
