// RightKitHub's public API (HubContext) exposes the engine client contract, so the hub re-exports it.
// The native-backed RightKitEngine (SupervisedEngine) is not re-exported: an app that supervises an
// engine imports it explicitly and links the native archive; the hub and its tests need neither.
@_exported import RightKitEngineClient
