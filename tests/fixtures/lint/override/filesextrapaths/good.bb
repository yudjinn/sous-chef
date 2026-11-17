# In the .bb itself a late ${THISDIR} is still the recipe's directory.
FILESEXTRAPATHS:prepend = "${THISDIR}/files:"
# OE-core's systemd-boot_259.5.bb:2 and cross-localedef-native_2.43.bb:20: `=.` prepends
# without a space, keeping the default; `${FILE_DIRNAME}` is the recipe's own directory.
FILESEXTRAPATHS =. "${FILE_DIRNAME}/systemd:"
FILESEXTRAPATHS =. "${FILE_DIRNAME}/${PN}:${FILE_DIRNAME}/glibc:"
# `.=` appends without a space; the leading colon separates the path.
FILESEXTRAPATHS .= ":${FILE_DIRNAME}/late"
