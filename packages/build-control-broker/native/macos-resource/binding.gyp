{
  "variables": {
    "enable_lto": "false",
    "enable_thin_lto": "false",
    "lto_jobs": ""
  },
  "targets": [
    {
      "target_name": "rightkit_macos_resource",
      "conditions": [
        ["OS=='mac'", {
          "sources": ["macos_resource.c"],
          "xcode_settings": {
            "MACOSX_DEPLOYMENT_TARGET": "13.0",
            "CLANG_CXX_LANGUAGE_STANDARD": "c11",
            "CLANG_CXX_LIBRARY": "libc++",
            "OTHER_CFLAGS": ["-Wall", "-Wextra", "-Werror"]
          }
        }, {
          "sources": [],
          "type": "none"
        }]
      ],
      "defines": ["NAPI_VERSION=8"]
    }
  ]
}
