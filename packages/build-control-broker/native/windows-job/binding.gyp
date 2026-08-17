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
          "libraries": ["kernel32.lib"],
          "msvs_settings": {
            "VCCLCompilerTool": {
              "WholeProgramOptimization": "false"
            },
            "VCLinkerTool": {
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
