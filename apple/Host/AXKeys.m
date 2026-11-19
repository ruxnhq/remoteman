#import "AXKeys.h"

AXError RMPostKey(AXUIElementRef application, CGKeyCode key, bool down) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    return AXUIElementPostKeyboardEvent(application, 0, key, down);
#pragma clang diagnostic pop
}

#import <IOKit/IOKitLib.h>
#import <IOKit/hidsystem/IOHIDLib.h>
#import <IOKit/hidsystem/IOHIDShared.h>
#import <IOKit/hidsystem/ev_keymap.h>

bool RMHIDKey(uint16_t key, bool down, bool shift) {
    static io_connect_t connection = 0;
    if (!connection) {
        io_service_t service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching(kIOHIDSystemClass));
        if (!service) return false;
        kern_return_t opened = IOServiceOpen(service, mach_task_self(), kIOHIDParamConnectType, &connection);
        IOObjectRelease(service);
        if (opened != KERN_SUCCESS) { connection = 0; return false; }
    }
    NXEventData event;
    bzero(&event, sizeof(event));
    event.key.keyCode = key;
    event.key.origCharSet = event.key.charSet = NX_ASCIISET;
    IOGPoint where = {0, 0};
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    kern_return_t posted = IOHIDPostEvent(connection, down ? NX_KEYDOWN : NX_KEYUP, where, &event, kNXEventDataVersion, shift ? NX_SHIFTMASK : 0, 0);
#pragma clang diagnostic pop
    return posted == KERN_SUCCESS;
}
