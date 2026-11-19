#import "AXKeys.h"

AXError RMPostKey(AXUIElementRef application, CGKeyCode key, bool down) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
    return AXUIElementPostKeyboardEvent(application, 0, key, down);
#pragma clang diagnostic pop
}
