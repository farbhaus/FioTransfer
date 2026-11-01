#import <Cocoa/Cocoa.h>
#include "fio_export.h"

// Finder's right-click "Upload with FioTransfer" and "Create Watch Folder in
// FioTransfer": the NSServices in Info.plist name these methods.

@interface FioServices : NSObject
@end
@implementation FioServices
static void fioServe(NSPasteboard *pb, int kind, NSString **err) {
	NSArray<NSURL *> *urls = [pb readObjectsForClasses:@[NSURL.class] options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}];
	NSMutableArray<NSString *> *paths = [NSMutableArray new];
	for (NSURL *u in urls) {
		NSString *p = u.filePathURL.path; // Finder may send file reference URLs
		if (p.length) [paths addObject:p];
	}
	if (!paths.count) {
		if (err) *err = @"No files";
		return;
	}
	NSData *data = [NSJSONSerialization dataWithJSONObject:paths options:0 error:nil];
	NSString *json = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
	goService(kind, (char *)json.UTF8String);
}
- (void)fioUpload:(NSPasteboard *)pb userData:(NSString *)data error:(NSString **)err { fioServe(pb, 0, err); }
- (void)fioWatch:(NSPasteboard *)pb userData:(NSString *)data error:(NSString **)err { fioServe(pb, 1, err); }
@end

static FioServices *fioServices;

// fioServicesInit runs before eframe starts the app. macOS launches the app to
// handle a service when it isn't running, so the provider must be in place
// as the app finishes launching, before the window and the engine exist.
void fioServicesInit(void) {
	[[NSNotificationCenter defaultCenter] addObserverForName:NSApplicationWillFinishLaunchingNotification object:nil queue:nil usingBlock:^(NSNotification *n) {
		fioServices = [FioServices new];
		NSApp.servicesProvider = fioServices;
		NSUpdateDynamicServices(); // pick up the Info.plist entries of a new version
	}];
}
