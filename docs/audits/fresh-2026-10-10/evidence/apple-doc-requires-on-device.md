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
  "identifier" : "/documentation/Speech/SFSpeechRecognitionRequest/requiresOnDeviceRecognition",
  "metadataVersion" : "0.1.0",
  "role" : "Instance Property",
  "symbol" : {
    "kind" : "Instance Property",
    "modules" : [
      "Speech"
    ],
    "preciseIdentifier" : "c:objc(cs)SFSpeechRecognitionRequest(py)requiresOnDeviceRecognition"
  },
  "title" : "requiresOnDeviceRecognition"
}
-->

# requiresOnDeviceRecognition

A Boolean value that determines whether a request must keep its audio data on the device.

```
var requiresOnDeviceRecognition: Bool { get set }
```

## Discussion

Set this property to `true` to prevent an [`SFSpeechRecognitionRequest`](/documentation/Speech/SFSpeechRecognitionRequest) from sending audio over the network. However, on-device requests won’t be as accurate.

> Note:
> The request only honors this setting if the ``doc://com.apple.speech/documentation/Speech/SFSpeechRecognizer/supportsOnDeviceRecognition`` (``doc://com.apple.speech/documentation/Speech/SFSpeechRecognizer``) property is also `true`.

---

Copyright &copy; 2026 Apple Inc. All rights reserved. | [Terms of Use](https://www.apple.com/legal/internet-services/terms/site.html) | [Privacy Policy](https://www.apple.com/privacy/privacy-policy)