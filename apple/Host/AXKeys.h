// A key press sent to one application through the accessibility interface. The call is an old
// one that Swift no longer offers; it is what reaches the login window's password field.
#import <ApplicationServices/ApplicationServices.h>

AXError RMPostKey(AXUIElementRef application, CGKeyCode key, bool down);
