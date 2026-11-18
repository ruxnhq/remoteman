// A virtual display through CoreGraphics' private CGVirtualDisplay classes (macOS 14+), the
// mechanism display tools use for headless Macs. The classes are looked up at run time, so
// nothing private is linked; every call fails cleanly if they're missing.
// Adapted from espalier's VirtualDisplayShim.

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>

NS_ASSUME_NONNULL_BEGIN

@interface RMVirtualDisplay : NSObject

/// Whether this macOS has CGVirtualDisplay.
+ (BOOL)isSupported;

/// A new display named `name` that can grow up to `maxWidth`×`maxHeight` pixels.
/// `serial` tells displays apart: two live displays with the same one can't coexist.
- (nullable instancetype)initWithName:(NSString *)name maxWidth:(uint32_t)maxWidth maxHeight:(uint32_t)maxHeight serial:(uint32_t)serial;

/// The CGDirectDisplayID, or 0.
@property (readonly) CGDirectDisplayID displayID;

/// One mode of `width`×`height` pixels at `refreshRate` Hz. With `hiDPI`, the mode is
/// half that size in points with a 2× backing store.
- (BOOL)applyWidth:(uint32_t)width height:(uint32_t)height refreshRate:(double)refreshRate hiDPI:(BOOL)hiDPI;

/// For finding out what the system offers (not used for a client yet): several modes at once,
/// each @[width, height, refreshRate], with a transfer function (0: the ordinary initialiser)
/// and a refresh deadline in seconds (0: left alone).
- (BOOL)applyModes:(NSArray<NSArray<NSNumber *> *> *)modes transferFunction:(uint32_t)transferFunction refreshDeadline:(double)deadline;

@end

NS_ASSUME_NONNULL_END
