#import "CloudClipboardClient.h"
static void ClipboardRequest(NSString *method, NSString *path, NSDictionary *payload, NSString *token, MSIMECloudClipboardCompletion completion) { if (!token.length || path.length == 0) { if(completion) completion(nil,400,nil); return; } NSMutableURLRequest *r=[NSMutableURLRequest requestWithURL:[NSURL URLWithString:[@"https://api.msime.app" stringByAppendingString:path]]]; r.HTTPMethod=method; if(payload) { r.HTTPBody=[NSJSONSerialization dataWithJSONObject:payload options:0 error:nil]; [r setValue:@"application/json" forHTTPHeaderField:@"Content-Type"]; } [r setValue:[@"Bearer " stringByAppendingString:token] forHTTPHeaderField:@"Authorization"]; [[[NSURLSession sharedSession] dataTaskWithRequest:r completionHandler:^(NSData*d,NSURLResponse*response,NSError*e){ dispatch_async(dispatch_get_main_queue(), ^{if(completion)completion(d,[(NSHTTPURLResponse*)response statusCode],e);});}]resume]; }
void MSIMEFetchCloudClipboard(NSString *search,NSString *token,MSIMECloudClipboardCompletion c){ if(search.length>1024){if(c)c(nil,400,nil);return;} NSString *q=[search stringByAddingPercentEncodingWithAllowedCharacters:NSCharacterSet.URLQueryAllowedCharacterSet]; ClipboardRequest(@"GET",[NSString stringWithFormat:@"/v1/users/me/clipboard?q=%@",q?:@""],nil,token,c); }
void MSIMESetCloudClipboardEnabled(BOOL enabled,NSString *token,MSIMECloudClipboardCompletion c){ClipboardRequest(@"PUT",@"/v1/users/me/clipboard/settings",@{@"enabled":@(enabled)},token,c);}
void MSIMEAddCloudClipboard(NSString *text,NSString *token,MSIMECloudClipboardCompletion c){if(text.length==0||text.length>4000){if(c)c(nil,400,nil);return;}ClipboardRequest(@"POST",@"/v1/users/me/clipboard",@{@"text":text},token,c);}
void MSIMERemoveCloudClipboard(NSString *itemID, NSString *token, MSIMECloudClipboardCompletion completion) {
    if (![itemID isKindOfClass:NSString.class] || !itemID.length ||
        [itemID isEqualToString:@"."] || [itemID isEqualToString:@".."]) {
        if (completion) completion(nil, 400, nil);
        return;
    }
    NSCharacterSet *allowed = [NSCharacterSet characterSetWithCharactersInString:
        @"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"];
    NSString *component = [itemID stringByAddingPercentEncodingWithAllowedCharacters:allowed];
    if (!component.length) {
        if (completion) completion(nil, 400, nil);
        return;
    }
    ClipboardRequest(@"DELETE", [@"/v1/users/me/clipboard/" stringByAppendingString:component], nil, token, completion);
}
