#import "VirtualDisplay.h"

// The private interfaces, as declared by CoreGraphics (macOS 14+). Only declared here so
// the compiler knows the selectors; the classes come from NSClassFromString.
@interface CGVirtualDisplayDescriptor : NSObject
@property (retain, nonatomic) dispatch_queue_t queue;
@property (retain, nonatomic) NSString *name;
@property (nonatomic) unsigned int maxPixelsHigh;
@property (nonatomic) unsigned int maxPixelsWide;
@property (nonatomic) CGSize sizeInMillimeters;
@property (nonatomic) unsigned int productID;
@property (nonatomic) unsigned int vendorID;
@property (nonatomic) unsigned int serialNum;
@property (copy, nonatomic) void (^terminationHandler)(id, id);
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate;
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate transferFunction:(unsigned int)transferFunction;
@end

@interface CGVirtualDisplaySettings : NSObject
@property (retain, nonatomic) NSArray *modes;
@property (nonatomic) unsigned int hiDPI;
@property (nonatomic) double refreshDeadline;
@end

@interface CGVirtualDisplay : NSObject
@property (readonly, nonatomic) unsigned int displayID;
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@end

@implementation RMVirtualDisplay {
    id _display; // CGVirtualDisplay; the display lives as long as this object
}

+ (BOOL)isSupported {
    return NSClassFromString(@"CGVirtualDisplay") && NSClassFromString(@"CGVirtualDisplayDescriptor")
        && NSClassFromString(@"CGVirtualDisplaySettings") && NSClassFromString(@"CGVirtualDisplayMode");
}

- (instancetype)initWithName:(NSString *)name maxWidth:(uint32_t)maxWidth maxHeight:(uint32_t)maxHeight serial:(uint32_t)serial {
    if (!(self = [super init]) || ![RMVirtualDisplay isSupported]) return nil;
    CGVirtualDisplayDescriptor *d = [[NSClassFromString(@"CGVirtualDisplayDescriptor") alloc] init];
    d.queue = dispatch_queue_create("remoteman.virtual-display", DISPATCH_QUEUE_SERIAL);
    d.name = name;
    d.maxPixelsWide = maxWidth;
    d.maxPixelsHigh = maxHeight;
    // A 13" 4:3 panel; only used for the reported physical size.
    d.sizeInMillimeters = CGSizeMake(262, 197);
    d.vendorID = 0x524D; // "RM": its own, so it never collides with espalier's display
    d.productID = 1;
    d.serialNum = serial;
    d.terminationHandler = ^(id a, id b) {};
    _display = [[NSClassFromString(@"CGVirtualDisplay") alloc] initWithDescriptor:d];
    if (!_display) return nil;
    return self;
}

- (CGDirectDisplayID)displayID {
    return _display ? ((CGVirtualDisplay *)_display).displayID : 0;
}

- (BOOL)applyWidth:(uint32_t)width height:(uint32_t)height refreshRate:(double)refreshRate hiDPI:(BOOL)hiDPI {
    if (!_display) return NO;
    uint32_t w = hiDPI ? width / 2 : width, h = hiDPI ? height / 2 : height;
    CGVirtualDisplayMode *mode = [[NSClassFromString(@"CGVirtualDisplayMode") alloc] initWithWidth:w height:h refreshRate:refreshRate];
    CGVirtualDisplaySettings *s = [[NSClassFromString(@"CGVirtualDisplaySettings") alloc] init];
    s.modes = @[mode];
    s.hiDPI = hiDPI ? 1 : 0;
    return [(CGVirtualDisplay *)_display applySettings:s];
}

- (BOOL)applyModes:(NSArray<NSArray<NSNumber *> *> *)modes transferFunction:(uint32_t)transferFunction refreshDeadline:(double)deadline {
    if (!_display) return NO;
    NSMutableArray *list = [NSMutableArray array];
    Class cls = NSClassFromString(@"CGVirtualDisplayMode");
    for (NSArray<NSNumber *> *m in modes) {
        CGVirtualDisplayMode *mode = nil;
        if (transferFunction != 0 && [cls instancesRespondToSelector:@selector(initWithWidth:height:refreshRate:transferFunction:)]) {
            mode = [[cls alloc] initWithWidth:m[0].unsignedIntValue height:m[1].unsignedIntValue refreshRate:m[2].doubleValue transferFunction:transferFunction];
        } else {
            mode = [[cls alloc] initWithWidth:m[0].unsignedIntValue height:m[1].unsignedIntValue refreshRate:m[2].doubleValue];
        }
        if (mode) [list addObject:mode];
    }
    CGVirtualDisplaySettings *s = [[NSClassFromString(@"CGVirtualDisplaySettings") alloc] init];
    s.modes = list;
    s.hiDPI = 0;
    if (deadline > 0 && [s respondsToSelector:@selector(setRefreshDeadline:)]) s.refreshDeadline = deadline;
    return [(CGVirtualDisplay *)_display applySettings:s];
}

@end
