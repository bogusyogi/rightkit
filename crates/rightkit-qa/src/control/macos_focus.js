// Constant JXA program: no AppleEvents, shell, interpolated source, or temporary script.
// Native probe required: NSWorkspace callback & NSFileHandle EOF delivery on osascript.
ObjC.import('Cocoa');
ObjC.import('CoreFoundation');
var workspace = $.NSWorkspace.sharedWorkspace;
var workspaceCenter = workspace.notificationCenter;
var defaultCenter = $.NSNotificationCenter.defaultCenter;
var input = $.NSFileHandle.fileHandleWithStandardInput;
var output = $.NSFileHandle.fileHandleWithStandardOutput;
var observer;
var sequence = 0;
var stopping = false;
var ready = false;
function emit(kind, application) {
    var frame = {version: 1, sequence: sequence++, kind: kind};
    if (sequence > 65536) throw new Error('focus event limit');
    if (application) {
        frame.pid = Number(application.processIdentifier);
        frame.name = ObjC.unwrap(application.localizedName);
        frame.executable = ObjC.unwrap(application.executableURL.path);
        if (!(frame.pid > 0) || !frame.name || !frame.executable) throw new Error('unreadable focus identity');
    }
    output.writeData($(JSON.stringify(frame) + '\n').dataUsingEncoding($.NSUTF8StringEncoding));
}
function fail() {
    try { emit('invalid'); } catch (_) {}
    workspaceCenter.removeObserver(observer);
    defaultCenter.removeObserver(observer);
    $.CFRunLoopStop($.CFRunLoopGetCurrent());
}
ObjC.registerSubclass({
    name: 'RightKitFocusObserver', superclass: 'NSObject', methods: {
        'activated:': {types: ['void', ['id']], implementation: function(note) {
            try {
                var app = note.userInfo.objectForKey($.NSWorkspaceApplicationKey);
                if (!app || app.isNil()) throw new Error('missing notification application');
                emit('focus', app);
            } catch (_) { fail(); }
        }},
        'coverageLost:': {types: ['void', ['id']], implementation: function(_) { fail(); }},
        'started:': {types: ['void', ['id']], implementation: function(_) {
            try {
                // This selector executes only after entering the native main runloop.
                emit('focus', workspace.frontmostApplication);
                emit('ready');
                ready = true;
            } catch (_) { fail(); }
        }},
        'inputEnded:': {types: ['void', ['id']], implementation: function(note) {
            try {
                var data = note.userInfo.objectForKey($.NSFileHandleNotificationDataItem);
                if (!data || data.isNil() || Number(data.length) !== 0 || !ready) throw new Error('invalid shutdown');
                if (!stopping) {
                    stopping = true;
                    // One queued main-loop turn drains notifications already pending at EOF.
                    observer.performSelectorWithObjectAfterDelay('finished:', null, 0);
                }
            } catch (_) { fail(); }
        }},
        'finished:': {types: ['void', ['id']], implementation: function(_) {
            try {
                emit('focus', workspace.frontmostApplication);
                workspaceCenter.removeObserver(observer);
                defaultCenter.removeObserver(observer);
                emit('stopped');
                $.CFRunLoopStop($.CFRunLoopGetCurrent());
            } catch (_) { fail(); }
        }}
    }
});
observer = $.RightKitFocusObserver.alloc.init;
workspaceCenter.addObserverSelectorNameObject(observer, 'activated:', $.NSWorkspaceDidActivateApplicationNotification, null);
workspaceCenter.addObserverSelectorNameObject(observer, 'coverageLost:', $.NSWorkspaceWillSleepNotification, null);
workspaceCenter.addObserverSelectorNameObject(observer, 'coverageLost:', $.NSWorkspaceDidWakeNotification, null);
workspaceCenter.addObserverSelectorNameObject(observer, 'coverageLost:', $.NSWorkspaceSessionDidResignActiveNotification, null);
workspaceCenter.addObserverSelectorNameObject(observer, 'coverageLost:', $.NSWorkspaceSessionDidBecomeActiveNotification, null);
defaultCenter.addObserverSelectorNameObject(observer, 'inputEnded:', $.NSFileHandleReadCompletionNotification, input);
input.readInBackgroundAndNotify;
observer.performSelectorWithObjectAfterDelay('started:', null, 0);
$.CFRunLoopRun();
// Remove retained observers even if CFRunLoopRun returned without our stop selector.
workspaceCenter.removeObserver(observer);
defaultCenter.removeObserver(observer);
