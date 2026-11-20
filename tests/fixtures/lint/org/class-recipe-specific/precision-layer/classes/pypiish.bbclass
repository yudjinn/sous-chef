# lint-precision: trimmed from OE-core (kirkstone) classes/pypi.bbclass:21-23 and
# classes/cross-canadian.bbclass:10.
EXCLUDE_FROM_WORLD = "1"
SECTION = "devel/python"
S = "${WORKDIR}/${PYPI_PACKAGE}-${PV}"
