## Symbols are not loading when when using the dynamic loader
Modern `gdb` variants will place internal breakpoints to detect a shared library being loaded. The location of these breakpoints
depend on weather `gdb` was able to pinpoint a set of static probes placed on `ld.so`. If detecting the probes was successful,
the breakpoints are going to be set on a `glibc` internal functions. Since `dlopen-rs` does not use `glibc` to load shared libraries,
loading new objects will go unnoticed by `gdb`. For more details, see [Issue #10](https://github.com/weizhiao/rust-dlopen/issues/10).

We provide a `gdb` script that can be used when starting up `gdb` to manually add debug symbols:

```sh
gdb -x scripts/reload_sos_on_r_brk.py /path/to/binary
```
Make sure that you have `elftools` installed via `pip` as well as the debug information for `glibc` either through the official packages,
or `debuginfod`.

#### Packages providing debugging symbols

| Distro family   | Package name |
| :-------------- | :----------: |
| Ubuntu/Debian   | libc6-dbg    |
| RHEL/Fedora     | glibc-debug  |
| Arch vaiants    | glibc-debug  |

#### Notes on ubuntu

After installing the debug symbols you may need to modify the directory where GDB searches for debug files:
```sh 
set debug-file-directory /usr/lib/debug
```
