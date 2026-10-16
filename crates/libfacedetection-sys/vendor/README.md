# libfacedetection

`facedetectcnn*.{h,cpp}` and `LICENSE` are copied unchanged from
<https://github.com/ShiqiYu/libfacedetection>, `src/` at commit
acf7b254121927e7dced30e233a2e03119f28ea2 (2026-06-28). nixpkgs does not
package it, and its CMake build would add nothing but an export header,
which `facedetection_export.h` stands in for.

To move it: copy the four files from a newer `src/` over these and update
the commit above.
