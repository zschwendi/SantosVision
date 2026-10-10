# SantosVision native visionOS checkpoint

This is a native SwiftUI/CompositorServices app that renders a stereo 3D calibration world with Metal and predicted ARKit headset pose. It is an engineering checkpoint for the requested surrounding GTA world with head tracking. **It does not launch GTA V.** The Play GTA V button is disabled and explains the missing web-port package and immersive camera/rendering bridge.

## Build

Requires full Xcode with visionOS SDK and XcodeGen. The deployment target is visionOS 26.0. The bundle ID is `com.zephyrstudios.santosvision`, separate from upstream Muguet. From the repository root:

```sh
./make-visionos.sh diagnostic simulator
./make-visionos.sh diagnostic device
```

Projects, derived data and app bundles stay under `target/visionos-xcode/`. The unsigned generic device build verifies compilation; it does not install on Vision Pro. The simulator build checks compilation and launcher behavior. A physical Vision Pro is required to verify real world tracking, stereo alignment, depth reprojection and performance.

```sh
./make-visionos.sh game simulator
```

Game mode deliberately exits 65 with the missing-runtime explanation. No stub game success is reported. No mode boots a simulator or installs to a physical headset.

## Tracking test

Open SantosVision, choose **Test headset tracking**, and allow the immersive space. A floor grid and colored 3D markers surround the initial tracked headset position. Turn and translate your head slightly: geometry should remain stationary, near objects should shift against distant ones, and solid objects should occlude correctly. Choose **Close tracking test**, or use the Digital Crown to leave. This scene has no GTA assets.

The renderer queries the predicted device anchor after the compositor's optimal input time, uses the drawable's presentation timestamp, assigns the same anchor to the compositor, and derives each eye's view matrix and projection from the supplied view transform. It stores reverse-Z depth and renders each eye to its assigned texture, slice and viewport. Missing tracking suspends geometry drawing. Paused layers receive no frames; invalidation and explicit closure stop rendering and ARKit. The renderer uses an independent thread and bounds GPU synchronization waits for shutdown.

The simulator cannot demonstrate physical headset tracking. If `WorldTrackingProvider.isSupported` is false, the app reports that a physical Vision Pro is required. Compilation and launcher screenshots are not a claim that GTA or physical headset tracking is working.

## Game integration

A retail PC/console GTA V install or console ISO is not this fork's required web-port package. The existing loader expects the compatible ZIP layout `data/` plus `b/<build>/game.wasm`; a normal retail install does not provide that WebAssembly runtime.

Even with the right package, native GTA immersion remains unfinished. See [IMMERSIVE-BRIDGE.md](IMMERSIVE-BRIDGE.md) and the proposed [ImmersiveBridge.h](ImmersiveBridge.h) for the required pose, stereo-camera, Metal color/depth destination, lifetime and frame-snapshot contract. The contract header is not linked into this diagnostic.

Apple references: [CompositorServices](https://developer.apple.com/documentation/compositorservices), [Metal immersive rendering](https://developer.apple.com/documentation/compositorservices/drawing-fully-immersive-content-using-metal), [predicted ARKit device anchor](https://developer.apple.com/documentation/arkit/worldtrackingprovider/querydeviceanchor(attimestamp:)), [per-eye projection](https://developer.apple.com/documentation/compositorservices/layerrenderer/drawable/computeprojection(convention:viewindex:)).
