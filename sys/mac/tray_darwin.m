#import <Cocoa/Cocoa.h>
#include "fio_export.h"


@interface FioTrayTarget : NSObject
- (void)act:(NSMenuItem *)sender;
@end
@implementation FioTrayTarget
- (void)act:(NSMenuItem *)sender { goTrayAction((int)sender.tag); }
@end

static NSStatusItem *fioItem;
static NSMenuItem *fioStatusLine;
static NSMenuItem *fioMoreItem; // "3 more" under the transfers that fit
static NSMutableArray<NSMenuItem *> *fioJobItems;
static NSMenuItem *fioLoginItem;
static NSMenuItem *fioUpdateItem;
static NSMenuItem *fioCheckItem;
static FioTrayTarget *fioTarget;

BOOL fioQuickVisible(void); // quick_darwin.m

// FioJobView is a running transfer in the menu: arrow, name and percentage,
// a bar, then size, speed and time left. Text lines up with the item titles
// (after the checkmark column); the arrow sits in that column.
@interface FioJobView : NSView
@end
@implementation FioJobView {
	NSImageView *_arrow;
	NSTextField *_name, *_pct, *_detail;
	double _progress; // 0…1, <0 while the size isn't known
}
static const CGFloat fioJobIndent = 22, fioJobRight = 14, fioJobPct = 44;

static NSTextField *fioJobLabel(CGFloat size, NSColor *color) {
	NSTextField *t = [NSTextField labelWithString:@""];
	t.font = [NSFont monospacedDigitSystemFontOfSize:size weight:NSFontWeightRegular];
	t.textColor = color;
	t.lineBreakMode = NSLineBreakByTruncatingMiddle;
	return t;
}

- (instancetype)init {
	CGFloat w = 300;
	self = [super initWithFrame:NSMakeRect(0, 0, w, 56)];
	self.autoresizingMask = NSViewWidthSizable;
	_arrow = [[NSImageView alloc] initWithFrame:NSMakeRect(fioJobIndent - 15, 7, 12, 16)];
	_arrow.symbolConfiguration = [NSImageSymbolConfiguration configurationWithPointSize:11 weight:NSFontWeightSemibold];
	_arrow.contentTintColor = NSColor.secondaryLabelColor;
	_name = fioJobLabel(13, NSColor.labelColor);
	_name.frame = NSMakeRect(fioJobIndent, 6, w - fioJobIndent - fioJobRight - fioJobPct - 6, 17);
	_name.autoresizingMask = NSViewWidthSizable;
	_pct = fioJobLabel(13, NSColor.secondaryLabelColor);
	_pct.frame = NSMakeRect(w - fioJobRight - fioJobPct, 6, fioJobPct, 17);
	_pct.alignment = NSTextAlignmentRight;
	_pct.autoresizingMask = NSViewMinXMargin;
	_detail = fioJobLabel(11, NSColor.secondaryLabelColor);
	_detail.frame = NSMakeRect(fioJobIndent, 36, w - fioJobIndent - fioJobRight, 14);
	_detail.autoresizingMask = NSViewWidthSizable;
	for (NSView *v in @[_arrow, _name, _pct, _detail]) [self addSubview:v];
	return self;
}

- (BOOL)isFlipped { return YES; }

- (void)show:(NSDictionary *)j {
	NSString *sym = [j[@"up"] boolValue] ? @"arrow.up" : @"arrow.down";
	_arrow.image = [NSImage imageWithSystemSymbolName:sym accessibilityDescription:[j[@"up"] boolValue] ? @"Upload" : @"Download"];
	_name.stringValue = j[@"name"] ?: @"";
	_name.toolTip = _name.stringValue;
	_pct.stringValue = j[@"pct"] ?: @"";
	_detail.stringValue = j[@"detail"] ?: @"";
	_progress = [j[@"progress"] doubleValue];
	self.needsDisplay = YES;
}

- (void)drawRect:(NSRect)dirty {
	NSRect track = NSMakeRect(fioJobIndent, 28, NSWidth(self.bounds) - fioJobIndent - fioJobRight, 4);
	[NSColor.tertiaryLabelColor setFill];
	[[NSBezierPath bezierPathWithRoundedRect:track xRadius:2 yRadius:2] fill];
	if (_progress > 0) {
		NSRect done = track;
		done.size.width = MAX(4, NSWidth(track) * MIN(_progress, 1));
		[[NSColor colorWithSRGBRed:0x5b / 255.0 green:0x53 / 255.0 blue:1 alpha:1] setFill]; // brand
		[[NSBezierPath bezierPathWithRoundedRect:done xRadius:2 yRadius:2] fill];
	}
}
@end

static NSMenuItem *fioAdd(NSMenu *m, NSString *title, int tag, NSString *key) {
	NSMenuItem *it = [[NSMenuItem alloc] initWithTitle:title action:@selector(act:) keyEquivalent:key];
	it.target = fioTarget;
	it.tag = tag;
	[m addItem:it];
	return it;
}

// fioAppMenuAdd puts "Check for Updates…" under About in the app menu, which
// winit builds with no way to add items, and sends its Quit (⌘Q) through the
// app (tag 3), which asks first while transfers run. It's set before the run
// loop starts, so it's normally there already; tries is how many more times to
// look.
static void fioAppMenuAdd(int tries) {
	NSMenu *app = NSApp.mainMenu.itemArray.firstObject.submenu;
	if (!app) {
		if (tries > 0) dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC / 2), dispatch_get_main_queue(), ^{ fioAppMenuAdd(tries - 1); });
		return;
	}
	for (NSMenuItem *q in app.itemArray) {
		if (q.action == @selector(terminate:)) {
			q.target = fioTarget;
			q.action = @selector(act:);
			q.tag = 3;
		}
	}
	if ([app itemWithTag:6]) return;
	NSMenuItem *it = [[NSMenuItem alloc] initWithTitle:@"Check for Updates…" action:@selector(act:) keyEquivalent:@""];
	it.target = fioTarget;
	it.tag = 6;
	if ([app.itemArray.firstObject.title hasPrefix:@"About "]) {
		[app insertItem:it atIndex:1];
	} else {
		[app insertItem:[NSMenuItem separatorItem] atIndex:0];
		[app insertItem:it atIndex:0];
	}
}

void fioTrayInit(void) {
	dispatch_async(dispatch_get_main_queue(), ^{
		if (fioItem) return;
		fioTarget = [FioTrayTarget new];
		fioItem = [[NSStatusBar systemStatusBar] statusItemWithLength:NSVariableStatusItemLength];
		NSImage *img = [NSImage imageWithSystemSymbolName:@"arrow.up.arrow.down" accessibilityDescription:@"FioTransfer"];
		img.template = YES;
		fioItem.button.image = img;
		fioItem.button.imagePosition = NSImageLeft;
		NSMenu *m = [NSMenu new];
		m.autoenablesItems = NO;
		fioStatusLine = [[NSMenuItem alloc] initWithTitle:@"Idle" action:nil keyEquivalent:@""];
		fioStatusLine.enabled = NO;
		[m addItem:fioStatusLine];
		fioJobItems = [NSMutableArray new];
		fioMoreItem = [[NSMenuItem alloc] initWithTitle:@"" action:nil keyEquivalent:@""];
		fioMoreItem.enabled = NO;
		fioMoreItem.hidden = YES;
		[m addItem:fioMoreItem];
		[m addItem:[NSMenuItem separatorItem]];
		fioUpdateItem = fioAdd(m, @"Update", 5, @"");
		fioUpdateItem.hidden = YES;
		fioAdd(m, @"Download from Link…", 7, @"");
		fioAdd(m, @"Open FioTransfer", 1, @"");
		fioLoginItem = fioAdd(m, @"Open at Login", 2, @"");
		fioCheckItem = fioAdd(m, @"Check for Updates…", 6, @"");
		[m addItem:[NSMenuItem separatorItem]];
		fioAdd(m, @"Quit FioTransfer", 3, @"q");
		fioItem.menu = m;
		fioAppMenuAdd(5);
		// Re-opening the app (e.g. double-click in Finder) while hidden brings the window back.
		[[NSNotificationCenter defaultCenter] addObserverForName:NSApplicationDidBecomeActiveNotification object:nil queue:nil usingBlock:^(NSNotification *n) {
			if (fioQuickVisible()) return;
			for (NSWindow *w in NSApp.windows) {
				if (w.isVisible && w.canBecomeMainWindow) return;
			}
			goTrayAction(4);
		}];
	});
}

void fioTraySet(const char *status, const char *title, int login, const char *update) {
	NSString *s = [NSString stringWithUTF8String:status];
	NSString *t = [NSString stringWithUTF8String:title];
	NSString *u = [NSString stringWithUTF8String:update];
	dispatch_async(dispatch_get_main_queue(), ^{
		if (!fioItem) return;
		fioStatusLine.title = s;
		fioItem.button.title = t;
		fioLoginItem.state = login ? NSControlStateValueOn : NSControlStateValueOff;
		fioUpdateItem.hidden = u.length == 0;
		fioCheckItem.hidden = u.length > 0; // the update item takes its place
		if (u.length) fioUpdateItem.title = u;
	});
}

// fioTrayJobs shows running transfers under the status line, from trayJobs'
// JSON. Rows are reused, so an open menu updates in place.
void fioTrayJobs(const char *json) {
	NSData *data = [NSData dataWithBytes:json length:strlen(json)];
	dispatch_async(dispatch_get_main_queue(), ^{
		if (!fioItem) return;
		NSDictionary *o = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
		NSArray<NSDictionary *> *jobs = [o isKindOfClass:[NSDictionary class]] ? o[@"jobs"] : nil;
		if (![jobs isKindOfClass:[NSArray class]]) jobs = @[];
		NSMenu *m = fioItem.menu;
		while (fioJobItems.count > jobs.count) {
			[m removeItem:fioJobItems.lastObject];
			[fioJobItems removeLastObject];
		}
		for (NSUInteger i = 0; i < jobs.count; i++) {
			if (i == fioJobItems.count) {
				NSMenuItem *it = [NSMenuItem new];
				it.view = [FioJobView new];
				[m insertItem:it atIndex:[m indexOfItem:fioMoreItem]];
				[fioJobItems addObject:it];
			}
			[(FioJobView *)fioJobItems[i].view show:jobs[i]];
		}
		int more = [o[@"more"] intValue];
		fioMoreItem.hidden = more == 0;
		fioMoreItem.title = [NSString stringWithFormat:@"%d more", more];
	});
}

// Show the Dock icon only while a window is on screen.
void fioSyncDock(void) {
	dispatch_async(dispatch_get_main_queue(), ^{
		BOOL visible = NO;
		for (NSWindow *w in NSApp.windows) {
			if ((w.isVisible || w.isMiniaturized) && w.canBecomeMainWindow) { visible = YES; break; }
		}
		NSApplicationActivationPolicy want = visible ? NSApplicationActivationPolicyRegular : NSApplicationActivationPolicyAccessory;
		if (NSApp.activationPolicy != want) [NSApp setActivationPolicy:want];
	});
}

// Closing the window hides the app, as Wails' HideWindowOnClose did: the
// menu bar item and the transfers keep running, and activating the app (the
// menu, the Dock, Finder) brings the window back.
void fioHide(void) {
	dispatch_async(dispatch_get_main_queue(), ^{ [NSApp hide:nil]; });
}

void fioActivate(void) {
	dispatch_async(dispatch_get_main_queue(), ^{
		[NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
		[NSApp activateIgnoringOtherApps:YES];
	});
}

// While transfers run, tell macOS this is user-requested work: no App Nap
// throttling and no idle sleep, even when the window is hidden.
static id fioActivity;

void fioSetActive(int on) {
	dispatch_async(dispatch_get_main_queue(), ^{
		if (on && !fioActivity) {
			fioActivity = [[NSProcessInfo processInfo] beginActivityWithOptions:(NSActivityUserInitiated | NSActivityIdleSystemSleepDisabled)
			                                                             reason:@"Transfers in progress"];
		} else if (!on && fioActivity) {
			[[NSProcessInfo processInfo] endActivity:fioActivity];
			fioActivity = nil;
		}
	});
}
