# OE-core's clang/common.inc:1 and gcc-15.3.inc:11: an .inc included only by recipes of its
# own directory, where a late ${FILE_DIRNAME} is that directory.
FILESEXTRAPATHS =. "${FILE_DIRNAME}/clang:"
FILESEXTRAPATHS:prepend = "${THISDIR}/files:"
