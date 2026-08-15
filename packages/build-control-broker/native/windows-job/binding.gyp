{
  "targets": [
    {
      "target_name": "rightkit_windows_job",
      "conditions": [
        ["OS=='win'", {
          "sources": ["windows_job.c"],
          "libraries": ["kernel32.lib"]
        }, {
          "sources": [],
          "type": "none"
        }]
      ],
      "defines": ["NAPI_VERSION=8"]
    }
  ]
}
