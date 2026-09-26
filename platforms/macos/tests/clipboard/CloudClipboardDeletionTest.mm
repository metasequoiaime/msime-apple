#import "../../src/cloud/CloudClipboardClient.h"
#include <cassert>
int main() {
    @autoreleasepool {
        for (id itemID in @[@"", @".", @"..", NSNull.null, @42]) {
            __block BOOL completed = NO;
            MSIMERemoveCloudClipboard(itemID, @"synthetic-session", ^(NSData *data, NSInteger status, NSError *error) {
                assert(data == nil && status == 400 && error == nil);
                completed = YES;
            });
            assert(completed);
        }
        MSIMERemoveCloudClipboard(nil, @"synthetic-session", nil);
        // Invalid input is a synchronous no-op when the caller does not need a result.
        MSIMEFetchCloudClipboard([@"x" stringByPaddingToLength:1025 withString:@"x" startingAtIndex:0],
                                  @"synthetic-session", nil);
        MSIMEAddCloudClipboard([@"x" stringByPaddingToLength:4001 withString:@"x" startingAtIndex:0],
                               @"synthetic-session", nil);
        __block BOOL fetchCompleted = NO;
        MSIMEFetchCloudClipboard([@"x" stringByPaddingToLength:1025 withString:@"x" startingAtIndex:0],
                                  @"synthetic-session", ^(NSData *data, NSInteger status, NSError *error) {
            assert(data == nil && status == 400 && error == nil);
            fetchCompleted = YES;
        });
        assert(fetchCompleted);
        __block BOOL addCompleted = NO;
        MSIMEAddCloudClipboard(@"", @"synthetic-session", ^(NSData *data, NSInteger status, NSError *error) {
            assert(data == nil && status == 400 && error == nil);
            addCompleted = YES;
        });
        assert(addCompleted);
    }
}
