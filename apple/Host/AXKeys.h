// A key press sent to one application through the accessibility interface. The call is an old
// one that Swift no longer offers; it is what reaches the login window's password field.
#import <ApplicationServices/ApplicationServices.h>

AXError RMPostKey(AXUIElementRef application, CGKeyCode key, bool down);

// A key press put into the system's own keyboard stream (the HID system), as login-screen
// tools have long done it. Returns false if the HID system couldn't be opened or refused.
bool RMHIDKey(uint16_t key, bool down, bool shift);
