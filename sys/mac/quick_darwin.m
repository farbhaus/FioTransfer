#import <Cocoa/Cocoa.h>
#include "fio_export.h"

// "Download from Link…" in the menu bar menu: a small panel with a link field
// and where to save, so a link downloads without the main window. Panels can't
// become main, so it shows no Dock icon (fioSyncDock).

static NSColor *fioHex(unsigned hex) {
	return [NSColor colorWithSRGBRed:(hex >> 16 & 0xff) / 255.0 green:(hex >> 8 & 0xff) / 255.0 blue:(hex & 0xff) / 255.0 alpha:1];
}

// FioQuickPanel handles the editing shortcuts itself: without a Dock icon the
// app menu that carries them isn't in the menu bar.
@interface FioQuickPanel : NSPanel
@end
@implementation FioQuickPanel
- (BOOL)performKeyEquivalent:(NSEvent *)e {
	if ((e.modifierFlags & NSEventModifierFlagDeviceIndependentFlagsMask) == NSEventModifierFlagCommand) {
		NSDictionary<NSString *, NSString *> *sels = @{@"x": @"cut:", @"c": @"copy:", @"v": @"paste:", @"a": @"selectAll:", @"z": @"undo:", @"w": @"performClose:"};
		NSString *sel = sels[e.charactersIgnoringModifiers];
		if (sel && [NSApp sendAction:NSSelectorFromString(sel) to:nil from:self]) return YES;
	}
	return [super performKeyEquivalent:e];
}
@end

@interface FioQuick : NSObject <NSTextFieldDelegate, NSWindowDelegate>
@end

static const CGFloat qWidth = 460;
static FioQuickPanel *qPanel;
static NSStackView *qStack;
static NSTextField *qLink, *qPass, *qInfo, *qDetail;
static NSButton *qDest, *qSub, *qGo;
static NSView *qDot;
static NSProgressIndicator *qSpin;
static NSTimer *qTimer;
static FioQuick *qCtl;
static NSString *qDir = @"";
static int qSeq;      // the latest inspect; answers to older ones are dropped
static int qState;    // the shown answer (quickOK, quickPass, quickError), 0 while there's none
static BOOL qStarting;
static BOOL qPlaced;  // centered the first time, then it opens where it was left

static NSString *qTrim(NSString *s) { return [s stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet]; }

static void qSync(void) {
	qGo.enabled = qTrim(qLink.stringValue).length > 0 && qState != 3 && !qStarting;
}

// qFit sizes the panel to its rows, keeping its top edge in place.
static void qFit(void) {
	NSRect f = qPanel.frame;
	NSRect n = [qPanel frameRectForContentRect:NSMakeRect(0, 0, qWidth, qStack.fittingSize.height)];
	n.origin = NSMakePoint(f.origin.x, NSMaxY(f) - n.size.height);
	[qPanel setFrame:n display:YES];
}

static void qSetPass(BOOL on) {
	if (qPass.hidden != on) return;
	qPass.hidden = !on;
	if (!on) qPass.stringValue = @"";
	qFit();
}

static void qSetDir(NSString *dir) {
	qDir = dir;
	qDest.title = [[NSFileManager defaultManager] displayNameAtPath:dir];
	qDest.toolTip = dir;
}

// qShowInfo shows a link's name (or a message) and its file count and size.
// Only the name gives way when the line is too long.
static void qShowInfo(int state, NSString *name, NSString *detail, NSString *warn) {
	NSFont *font = [NSFont systemFontOfSize:12];
	NSColor *color = state == 1 ? NSColor.labelColor : state == 2 ? fioHex(0xf0cd33) : fioHex(0xf43350);
	NSMutableParagraphStyle *one = [NSMutableParagraphStyle new];
	one.lineBreakMode = NSLineBreakByTruncatingTail;
	qInfo.attributedStringValue = [[NSAttributedString alloc] initWithString:name attributes:@{
		NSFontAttributeName: state == 1 ? [NSFont systemFontOfSize:12 weight:NSFontWeightSemibold] : font,
		NSForegroundColorAttributeName: color,
		NSParagraphStyleAttributeName: one,
	}];
	NSMutableAttributedString *d = [[NSMutableAttributedString alloc] initWithString:detail
		attributes:@{NSFontAttributeName: font, NSForegroundColorAttributeName: NSColor.secondaryLabelColor}];
	if (warn.length) {
		[d appendAttributedString:[[NSAttributedString alloc] initWithString:[@" · " stringByAppendingString:warn]
			attributes:@{NSFontAttributeName: font, NSForegroundColorAttributeName: fioHex(0xf0cd33)}]];
	}
	qDetail.attributedStringValue = d;
	qDetail.hidden = d.length == 0;
	qInfo.superview.toolTip = d.length ? [NSString stringWithFormat:@"%@  %@", name, d.string] : name;
	qDot.hidden = state == 0;
	qDot.layer.backgroundColor = (state == 1 ? fioHex(0x21c17b) : color).CGColor;
}

static void qInspect(void) {
	[qTimer invalidate];
	qTimer = nil;
	NSString *link = qTrim(qLink.stringValue);
	int seq = ++qSeq;
	qState = 0;
	qShowInfo(0, @"", @"", @"");
	if (!link.length) {
		[qSpin stopAnimation:nil];
		qSetPass(NO);
	} else {
		[qSpin startAnimation:nil];
		goQuickInspect(seq, (char *)link.UTF8String, (char *)qPass.stringValue.UTF8String);
	}
	qSync();
}

// qYield gives the focus back to the app the panel opened over, unless a
// window of ours is still on screen.
static void qYield(void) {
	if (qPanel.visible) return;
	for (NSWindow *w in NSApp.windows) {
		if (w.visible && w.canBecomeMainWindow) return;
	}
	[NSApp hide:nil];
}

@implementation FioQuick
- (void)controlTextDidChange:(NSNotification *)n {
	if (n.object == qLink) qState = 0; // the answer was for another link
	qSync();
	[qTimer invalidate];
	qTimer = [NSTimer scheduledTimerWithTimeInterval:0.45 repeats:NO block:^(NSTimer *t) { qInspect(); }];
}

- (void)chooseDir:(id)sender {
	NSOpenPanel *p = [NSOpenPanel openPanel];
	p.canChooseFiles = NO;
	p.canChooseDirectories = YES;
	p.canCreateDirectories = YES;
	p.resolvesAliases = YES;
	p.prompt = @"Choose";
	p.directoryURL = [NSURL fileURLWithPath:qDir isDirectory:YES];
	[p beginSheetModalForWindow:qPanel completionHandler:^(NSModalResponse r) {
		if (r != NSModalResponseOK || !p.URL.path.length) return;
		qSetDir(p.URL.path);
		goQuickDest((char *)qDir.UTF8String);
	}];
}

- (void)cancel:(id)sender { [qPanel close]; }

- (void)start:(id)sender {
	NSString *link = qTrim(qLink.stringValue);
	if (!qGo.enabled || !link.length) return;
	qStarting = YES;
	qSync();
	goQuickStart((char *)link.UTF8String, (char *)qPass.stringValue.UTF8String, (char *)qDir.UTF8String, qSub.state == NSControlStateValueOn);
}

- (void)windowWillClose:(NSNotification *)n {
	[qTimer invalidate];
	qTimer = nil;
	qSeq++;
	dispatch_async(dispatch_get_main_queue(), ^{ qYield(); });
}
@end

static void qBuild(void) {
	if (qPanel) return;
	qCtl = [FioQuick new];
	qPanel = [[FioQuickPanel alloc] initWithContentRect:NSMakeRect(0, 0, qWidth, 200)
	                                          styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable
	                                            backing:NSBackingStoreBuffered
	                                              defer:YES];
	qPanel.title = @"Download from Link";
	qPanel.releasedWhenClosed = NO;
	qPanel.hidesOnDeactivate = NO; // stays up while a link is copied in another app
	qPanel.level = NSFloatingWindowLevel;
	qPanel.collectionBehavior = NSWindowCollectionBehaviorMoveToActiveSpace | NSWindowCollectionBehaviorFullScreenAuxiliary;
	qPanel.appearance = [NSAppearance appearanceNamed:NSAppearanceNameDarkAqua];
	qPanel.backgroundColor = fioHex(0x12141e);
	qPanel.titlebarAppearsTransparent = YES;
	qPanel.delegate = qCtl;
	[qPanel standardWindowButton:NSWindowMiniaturizeButton].hidden = YES;
	[qPanel standardWindowButton:NSWindowZoomButton].hidden = YES;

	qLink = [NSTextField textFieldWithString:@""];
	qLink.placeholderString = @"Frame.io, Smash, WeTransfer, Dropbox, Google Drive or Portal link";
	qLink.bezelStyle = NSTextFieldRoundedBezel;
	qLink.delegate = qCtl;
	qPass = [NSSecureTextField textFieldWithString:@""];
	qPass.placeholderString = @"Passphrase";
	qPass.bezelStyle = NSTextFieldRoundedBezel;
	qPass.delegate = qCtl;
	qPass.hidden = YES;

	// The link's name and size after a dot, or a spinner while it's checked.
	NSView *badge = [NSView new];
	qDot = [[NSView alloc] initWithFrame:NSMakeRect(4, 4, 8, 8)];
	qDot.wantsLayer = YES;
	qDot.layer.cornerRadius = 4;
	qDot.hidden = YES;
	qSpin = [[NSProgressIndicator alloc] initWithFrame:NSMakeRect(0, 0, 16, 16)];
	qSpin.style = NSProgressIndicatorStyleSpinning;
	qSpin.controlSize = NSControlSizeSmall;
	qSpin.displayedWhenStopped = NO;
	[badge addSubview:qDot];
	[badge addSubview:qSpin];
	qInfo = [NSTextField labelWithString:@""];
	qInfo.font = [NSFont systemFontOfSize:12];
	qInfo.lineBreakMode = NSLineBreakByTruncatingTail;
	qInfo.maximumNumberOfLines = 1;
	[qInfo setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];
	qDetail = [NSTextField labelWithString:@""];
	qDetail.font = qInfo.font;
	NSStackView *info = [NSStackView stackViewWithViews:@[badge, qInfo, qDetail]];
	info.spacing = 6;
	[info setCustomSpacing:8 afterView:qInfo];

	NSTextField *saveTo = [NSTextField labelWithString:@"Save to"];
	saveTo.textColor = NSColor.secondaryLabelColor;
	qDest = [NSButton buttonWithTitle:@"" image:[NSImage imageWithSystemSymbolName:@"folder" accessibilityDescription:nil] target:qCtl action:@selector(chooseDir:)];
	qDest.imagePosition = NSImageLeading;
	qDest.imageHugsTitle = YES;
	qDest.lineBreakMode = NSLineBreakByTruncatingMiddle;
	[qDest setContentCompressionResistancePriority:NSLayoutPriorityDefaultLow forOrientation:NSLayoutConstraintOrientationHorizontal];
	qSub = [NSButton checkboxWithTitle:@"New folder" target:nil action:nil];
	NSStackView *dest = [NSStackView stackViewWithViews:@[saveTo, qDest, qSub]];
	dest.spacing = 8;
	[dest setCustomSpacing:16 afterView:qDest];

	NSButton *cancel = [NSButton buttonWithTitle:@"Cancel" target:qCtl action:@selector(cancel:)];
	cancel.keyEquivalent = @"\e";
	qGo = [NSButton buttonWithTitle:@"Download" target:qCtl action:@selector(start:)];
	qGo.keyEquivalent = @"\r";
	qGo.bezelColor = fioHex(0x5b53ff);
	NSStackView *buttons = [NSStackView new];
	buttons.spacing = 12;
	[buttons addView:cancel inGravity:NSStackViewGravityTrailing];
	[buttons addView:qGo inGravity:NSStackViewGravityTrailing];

	qStack = [NSStackView stackViewWithViews:@[qLink, qPass, info, dest, buttons]];
	qStack.orientation = NSUserInterfaceLayoutOrientationVertical;
	qStack.alignment = NSLayoutAttributeLeading;
	qStack.detachesHiddenViews = YES;
	qStack.spacing = 10;
	qStack.edgeInsets = NSEdgeInsetsMake(12, 20, 20, 20);
	[qStack setCustomSpacing:18 afterView:info];
	[qStack setCustomSpacing:20 afterView:dest];
	qStack.translatesAutoresizingMaskIntoConstraints = NO;
	NSView *content = qPanel.contentView;
	[content addSubview:qStack];
	NSMutableArray<NSLayoutConstraint *> *cs = [NSMutableArray arrayWithArray:@[
		[qStack.topAnchor constraintEqualToAnchor:content.topAnchor],
		[qStack.leadingAnchor constraintEqualToAnchor:content.leadingAnchor],
		[qStack.trailingAnchor constraintEqualToAnchor:content.trailingAnchor],
		[badge.widthAnchor constraintEqualToConstant:16],
		[badge.heightAnchor constraintEqualToConstant:16],
		[qDest.widthAnchor constraintLessThanOrEqualToConstant:240],
		[cancel.widthAnchor constraintGreaterThanOrEqualToConstant:84],
		[qGo.widthAnchor constraintGreaterThanOrEqualToConstant:84],
		[qPass.widthAnchor constraintEqualToAnchor:qLink.widthAnchor],
	]];
	for (NSView *v in @[qLink, info, buttons]) {
		[cs addObject:[v.widthAnchor constraintEqualToAnchor:qStack.widthAnchor constant:-40]];
	}
	[NSLayoutConstraint activateConstraints:cs];
}

// fioQuickShow opens the panel, or brings it forward as it is. A panel that
// was closed starts over with link (may be empty) and dir.
void fioQuickShow(const char *link, const char *dir) {
	NSString *l = [NSString stringWithUTF8String:link];
	NSString *d = [NSString stringWithUTF8String:dir];
	dispatch_async(dispatch_get_main_queue(), ^{
		qBuild();
		BOOL fresh = !qPanel.visible;
		if (fresh) {
			qLink.stringValue = l;
			qSetPass(NO);
			qState = 0;
			qStarting = NO;
			[qSpin stopAnimation:nil];
			qShowInfo(0, @"", @"", @"");
			qSetDir(d);
			qSync();
			qFit();
			if (!qPlaced) {
				[qPanel center];
				qPlaced = YES;
			}
		}
		// Closing the main window hides the app (fioHide), and unhiding would
		// bring it back behind the panel. Put away what the hide took along
		// (canHide: not the menu bar icon; canBecomeMainWindow is NO while hidden).
		if (NSApp.hidden) {
			for (NSWindow *w in NSApp.windows) {
				if (w != qPanel && w.canHide && !w.isMiniaturized) [w orderOut:nil];
			}
			[NSApp unhideWithoutActivation];
		}
		[qPanel makeKeyAndOrderFront:nil];
		[NSApp activateIgnoringOtherApps:YES];
		if (fresh) {
			[qPanel makeFirstResponder:qLink]; // selects the link, so typing replaces it
			if (l.length) qInspect();
		}
	});
}

void fioQuickInfo(int seq, int state, const char *name, const char *detail, const char *warn) {
	NSString *n = [NSString stringWithUTF8String:name];
	NSString *d = [NSString stringWithUTF8String:detail];
	NSString *w = [NSString stringWithUTF8String:warn];
	dispatch_async(dispatch_get_main_queue(), ^{
		if (seq != qSeq) return;
		qState = state;
		[qSpin stopAnimation:nil];
		qShowInfo(state, n, d, w);
		if (state == 2) {
			qSetPass(YES);
			if (!qPass.stringValue.length) [qPanel makeFirstResponder:qPass];
		}
		qSync();
	});
}

void fioQuickStarted(const char *err) {
	NSString *e = [NSString stringWithUTF8String:err];
	dispatch_async(dispatch_get_main_queue(), ^{
		qStarting = NO;
		if (!qPanel.visible) return;
		if (!e.length) {
			[qPanel close];
			return;
		}
		qSeq++; // an answer still on its way would hide the error
		[qSpin stopAnimation:nil];
		qShowInfo(3, e, @"", @""); // the link itself is fine: Download stays enabled
		qSync();
	});
}

BOOL fioQuickVisible(void) { return qPanel.visible; }
