# :append right where the recipe itself sets the variable.
DEPENDS = "zlib"
DEPENDS += "openssl"
EXTRA_OEMAKE = "A=1"
EXTRA_OEMAKE .= "B"
