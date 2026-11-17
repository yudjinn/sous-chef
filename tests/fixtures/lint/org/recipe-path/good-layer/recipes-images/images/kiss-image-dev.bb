# Shaped after OE-core's core-image-minimal-dev.bb: an image only through the image it
# requires; unversioned like it.
require recipes-core/images/kiss-image.bb

IMAGE_FEATURES += "dev-pkgs"
