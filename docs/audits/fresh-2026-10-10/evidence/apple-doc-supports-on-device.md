<!--
{
  "availability" : [
    "iOS: 13.0.0 -",
    "iPadOS: 13.0.0 -",
    "macCatalyst: 13.0.0 -",
    "macOS: 10.15.0 -",
    "visionOS: 1.0.0 -"
  ],
  "documentType" : "symbol",
  "framework" : "Speech",
  "identifier" : "/documentation/Speech/SFSpeechRecognizer/supportsOnDeviceRecognition",
  "metadataVersion" : "0.1.0",
  "role" : "Instance Property",
  "symbol" : {
    "kind" : "Instance Property",
    "modules" : [
      "Speech"
    ],
    "preciseIdentifier" : "c:objc(cs)SFSpeechRecognizer(py)supportsOnDeviceRecognition"
  },
  "title" : "supportsOnDeviceRecognition"
}
-->

# supportsOnDeviceRecognition

A Boolean value that indicates whether the speech recognizer can operate without network access.

```
var supportsOnDeviceRecognition: Bool { get set }
```

## Discussion

An [`SFSpeechRecognitionRequest`](/documentation/Speech/SFSpeechRecognitionRequest) can only honor its [`requiresOnDeviceRecognition`](/documentation/Speech/SFSpeechRecognitionRequest/requiresOnDeviceRecognition) property if [`supportsOnDeviceRecognition`](/documentation/Speech/SFSpeechRecognizer/supportsOnDeviceRecognition) is `true`. If [`supportsOnDeviceRecognition`](/documentation/Speech/SFSpeechRecognizer/supportsOnDeviceRecognition) is `false`, the [`SFSpeechRecognizer`](/documentation/Speech/SFSpeechRecognizer) requires a network in order to recognize speech.

---

Copyright &copy; 2026 Apple Inc. All rights reserved. | [Terms of Use](https://www.apple.com/legal/internet-services/terms/site.html) | [Privacy Policy](https://www.apple.com/privacy/privacy-policy)