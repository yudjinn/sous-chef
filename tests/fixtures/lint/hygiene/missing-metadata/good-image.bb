# meta-kiss:recipes-core/images/kiss-image.bb (yocto-kiss cc621c5d): an image has no
# upstream, so no HOMEPAGE; LICENSE comes from image.bbclass (`LICENSE ?= "MIT"`).
#
# This recipe is a simplified form of core-image-minimal.

SUMMARY = "A simple, minimal image"

IMAGE_INSTALL = "packagegroup-core-boot dropbear sl"

IMAGE_LINGUAS = " "

inherit core-image
