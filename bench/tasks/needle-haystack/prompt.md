The current working directory contains a collection of single-file C
libraries. The file `stb_image.h` at the repository root is an image
decoding library of roughly 8,000 lines.

Using only the contents of `stb_image.h`, answer the following questions:

1. By default, what is the maximum image width/height the decoders will
   accept? Name the macro that controls this limit and its default numeric
   value.
2. When an image exceeds that limit, decoding fails with an error reason.
   Give the exact failure-reason strings that appear in the source for
   this case — both the short reason and the longer descriptive variant.
3. List every image file format this file can decode. For each format,
   give the exact name of the internal detection function the library uses
   to test whether input is in that format.

Your final answer must explicitly include the macro name, the numeric
value, both error strings, and every format paired with its detection
function name.
