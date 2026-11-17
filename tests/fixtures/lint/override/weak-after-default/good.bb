# ??= first, on another name, after unset, or repeated.
MY_A ??= "y"
MY_A = "x"
MY_B:qemux86 = "x"
MY_B ??= "y"
MY_C = "x"
unset MY_C
MY_C ??= "y"
MY_D ??= "a"
MY_D ??= "b"
MY_F[doc] = "d"
MY_F ??= "y"
