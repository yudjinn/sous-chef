# The file gives the referenced variable a value without the space.
MY_DEPS = "zlib"
DEPENDS:append = "${MY_DEPS}"
MY_FIRST = "file://first.patch"
SRC_URI:prepend = "${MY_FIRST}"
