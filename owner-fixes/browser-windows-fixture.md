
## Windows compile fix (run 38425528198, head 5e9a3916)
- rightkit-sandbox E0428: `FILE_FLAG_OPEN_REPARSE_POINT` declared twice in windows_appcontainer.rs (line 69 and a duplicate beside FILE_SHARE_READ). Removed the duplicate (identical value 0x0020_0000); no security logic touched. This was the only compile/clippy error in the log; rightkit-browser and rightkit-agent-tools failed only through it.
- E0046 `compare_and_save` is the rightkit-license `compile_fail` doctest (clock.rs:290) and is the expected failure ("compile fail ... ok"); not an error.
- Not verified on Windows locally (CI-only cargo); rustfmt --check clean.
