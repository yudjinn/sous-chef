# `=.` / `.=` mistakes, and a variable this file sets without the colon.
FILESEXTRAPATHS =. "${FILE_DIRNAME}/systemd:"
FILESEXTRAPATHS .= ":${FILE_DIRNAME}/late"
FILESEXTRAPATHS:qemux86 =. "${FILE_DIRNAME}/qemu:"
MYPATHS = "${THISDIR}/files"
FILESEXTRAPATHS:prepend = "${MYPATHS}:"
