#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

static void rightkit_dictation_copy_error(char *buffer, size_t capacity, NSString *message) {
    if (!buffer || capacity == 0) {
        return;
    }
    const char *utf8 = (message ?: @"unknown ScreenCaptureKit error").UTF8String;
    snprintf(buffer, capacity, "%s", utf8 ?: "unknown ScreenCaptureKit error");
}

int rightkit_dictation_capture_screen_excluding_app(const char *bundle_id_utf8,
                                            const char *output_path_utf8,
                                            char *error_buffer,
                                            size_t error_capacity) {
    if (@available(macOS 14.0, *)) {
        NSString *bundleID = bundle_id_utf8
            ? [NSString stringWithUTF8String:bundle_id_utf8]
            : @"";
        dispatch_semaphore_t finished = dispatch_semaphore_create(0);
        __block NSData *pngData = nil;
        __block NSError *captureError = nil;

        [SCShareableContent
            getShareableContentExcludingDesktopWindows:NO
                               onScreenWindowsOnly:YES
                                 completionHandler:^(SCShareableContent *content, NSError *error) {
            if (error || !content) {
                captureError = error ?: [NSError errorWithDomain:@"RightKitDictationScreenCapture"
                                                             code:1
                                                         userInfo:@{
                    NSLocalizedDescriptionKey: @"ScreenCaptureKit returned no shareable content"
                }];
                dispatch_semaphore_signal(finished);
                return;
            }

            CGDirectDisplayID mainDisplayID = CGMainDisplayID();
            SCDisplay *display = nil;
            for (SCDisplay *candidate in content.displays) {
                if (candidate.displayID == mainDisplayID) {
                    display = candidate;
                    break;
                }
            }
            display = display ?: content.displays.firstObject;
            if (!display) {
                captureError = [NSError errorWithDomain:@"RightKitDictationScreenCapture"
                                                    code:2
                                                userInfo:@{
                    NSLocalizedDescriptionKey: @"ScreenCaptureKit returned no display"
                }];
                dispatch_semaphore_signal(finished);
                return;
            }

            NSMutableArray<SCWindow *> *excluded = [NSMutableArray array];
            for (SCWindow *window in content.windows) {
                SCRunningApplication *owner = window.owningApplication;
                if ([owner.bundleIdentifier isEqualToString:bundleID]) {
                    [excluded addObject:window];
                }
            }

            SCContentFilter *filter =
                [[SCContentFilter alloc] initWithDisplay:display excludingWindows:excluded];
            SCStreamConfiguration *configuration = [[SCStreamConfiguration alloc] init];
            configuration.width = display.width;
            configuration.height = display.height;
            configuration.showsCursor = NO;

            [SCScreenshotManager
                captureImageWithFilter:filter
                         configuration:configuration
                     completionHandler:^(CGImageRef image, NSError *error) {
                if (image) {
                    NSBitmapImageRep *representation =
                        [[NSBitmapImageRep alloc] initWithCGImage:image];
                    pngData = [representation representationUsingType:NSBitmapImageFileTypePNG
                                                           properties:@{}];
                }
                captureError = error;
                if (!pngData && !captureError) {
                    captureError = [NSError errorWithDomain:@"RightKitDictationScreenCapture"
                                                       code:3
                                                   userInfo:@{
                        NSLocalizedDescriptionKey: @"ScreenCaptureKit returned no PNG image"
                    }];
                }
                dispatch_semaphore_signal(finished);
            }];
        }];

        if (dispatch_semaphore_wait(
                finished,
                dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC)) != 0) {
            rightkit_dictation_copy_error(
                error_buffer,
                error_capacity,
                @"ScreenCaptureKit timed out after 15 seconds"
            );
            return 4;
        }
        if (captureError || !pngData) {
            rightkit_dictation_copy_error(error_buffer, error_capacity, captureError.localizedDescription);
            return 5;
        }

        NSImage *image = [[NSImage alloc] initWithData:pngData];
        NSPasteboard *pasteboard = NSPasteboard.generalPasteboard;
        [pasteboard clearContents];
        if (!image || ![pasteboard writeObjects:@[image]]) {
            rightkit_dictation_copy_error(
                error_buffer,
                error_capacity,
                @"could not write ScreenCaptureKit image to clipboard"
            );
            return 6;
        }

        if (output_path_utf8) {
            NSString *path = [NSString stringWithUTF8String:output_path_utf8];
            NSError *writeError = nil;
            if (!path || ![pngData writeToFile:path
                                       options:NSDataWritingAtomic
                                         error:&writeError]) {
                rightkit_dictation_copy_error(
                    error_buffer,
                    error_capacity,
                    writeError.localizedDescription ?: @"could not write screenshot PNG"
                );
                return 7;
            }
        }
        return 0;
    }

    rightkit_dictation_copy_error(
        error_buffer,
        error_capacity,
        @"ScreenCaptureKit screenshots require macOS 14 or newer"
    );
    return 8;
}
