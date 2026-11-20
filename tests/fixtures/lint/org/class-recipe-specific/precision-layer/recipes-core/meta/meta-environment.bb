# Trimmed from OE-core (kirkstone) recipes-core/meta/meta-environment.bb:1-5: the same
# EXCLUDE_FROM_WORLD as the class (written with other spacing).
SUMMARY = "Package of environment files for SDK"
LICENSE = "MIT"
EXCLUDE_FROM_WORLD =   "1"

inherit pypiish
