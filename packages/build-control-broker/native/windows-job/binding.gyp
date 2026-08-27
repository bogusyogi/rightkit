{
  "variables": {
    "enable_lto": "false",
    "enable_thin_lto": "false",
    "lto_jobs": ""
  },
  "targets": [
    {
      "target_name": "rightkit_windows_job",
      "conditions": [
        ["OS=='win'", {
          "sources": ["windows_job.c"],
          "libraries": ["kernel32.lib", "advapi32.lib"],
          "msvs_settings": {
            "VCCLCompilerTool": {
              "AdditionalOptions!": ["-flto=thin", "-flto=full"],
              "WholeProgramOptimization": "false"
            },
            "VCLibrarianTool": {
              "AdditionalOptions!": ["-flto=thin", "-flto=full"]
            },
            "VCLinkerTool": {
              "AdditionalOptions!": ["-flto=thin", "-flto=full", "/opt:lldltojobs=<(lto_jobs)"],
              "LinkTimeCodeGeneration": 0
            }
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
