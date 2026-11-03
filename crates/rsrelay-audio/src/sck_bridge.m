#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreGraphics/CoreGraphics.h>
#import <AudioToolbox/AudioToolbox.h>

typedef void (*RsRelaySckAudioCallback)(
    void *user_ctx,
    const float *ch0,
    const float *ch1,
    size_t num_frames,
    uint32_t num_channels
);

@interface RsRelaySckOutputDelegate : NSObject <SCStreamOutput, SCStreamDelegate>
@property (nonatomic, assign) RsRelaySckAudioCallback callback;
@property (nonatomic, assign) void *userCtx;
@end

@implementation RsRelaySckOutputDelegate

- (void)stream:(SCStream *)stream
didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
        ofType:(SCStreamOutputType)type API_AVAILABLE(macos(13.0)) {
    if (type != SCStreamOutputTypeAudio) {
        return;
    }
    if (!CMSampleBufferDataIsReady(sampleBuffer)) {
        return;
    }

    CMItemCount numFrames = CMSampleBufferGetNumSamples(sampleBuffer);
    if (numFrames <= 0) {
        return;
    }

    char ablStorage[sizeof(AudioBufferList) + sizeof(AudioBuffer) * 8];
    memset(ablStorage, 0, sizeof(ablStorage));
    AudioBufferList *abl = (AudioBufferList *)ablStorage;
    CMBlockBufferRef blockBuffer = NULL;

    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer,
        NULL,
        abl,
        sizeof(ablStorage),
        NULL,
        NULL,
        kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
        &blockBuffer
    );

    if (status == noErr && abl->mNumberBuffers > 0) {
        const float *ch0 = (const float *)abl->mBuffers[0].mData;
        const float *ch1 = (abl->mNumberBuffers > 1 && abl->mBuffers[1].mData != NULL)
            ? (const float *)abl->mBuffers[1].mData
            : ch0;
        uint32_t numChannels = (uint32_t)abl->mNumberBuffers;

        RsRelaySckAudioCallback cb = self.callback;
        if (cb != NULL && ch0 != NULL) {
            cb(self.userCtx, ch0, ch1, (size_t)numFrames, numChannels);
        }
    }

    if (blockBuffer != NULL) {
        CFRelease(blockBuffer);
    }
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    (void)stream;
    (void)error;
}

@end

@interface RsRelaySckSession : NSObject
@property (nonatomic, strong) SCStream *stream API_AVAILABLE(macos(13.0));
@property (nonatomic, strong) RsRelaySckOutputDelegate *delegate;
@property (nonatomic, strong) dispatch_queue_t audioQueue;
@end

@implementation RsRelaySckSession
@end

bool rsrelay_sck_preflight_permission(void) {
    return CGPreflightScreenCaptureAccess();
}

bool rsrelay_sck_request_permission(void) {
    return CGRequestScreenCaptureAccess();
}

void *rsrelay_sck_stream_start(
    uint32_t sample_rate,
    uint32_t channels,
    bool exclude_current_process,
    RsRelaySckAudioCallback callback,
    void *user_ctx
) {
    if (@available(macOS 13.0, *)) {
        __block SCShareableContent *shareableContent = nil;
        __block NSError *contentError = nil;
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);

        [SCShareableContent getShareableContentExcludingDesktopWindows:NO
                                                   onScreenWindowsOnly:NO
                                                     completionHandler:^(SCShareableContent * _Nullable content, NSError * _Nullable error) {
            shareableContent = content;
            contentError = error;
            dispatch_semaphore_signal(sem);
        }];

        dispatch_time_t timeout = dispatch_time(DISPATCH_TIME_NOW, (int64_t)(3.0 * NSEC_PER_SEC));
        if (dispatch_semaphore_wait(sem, timeout) != 0 || shareableContent == nil || contentError != nil) {
            return NULL;
        }

        SCDisplay *display = shareableContent.displays.firstObject;
        if (display == nil) {
            return NULL;
        }

        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
        config.capturesAudio = YES;
        config.excludesCurrentProcessAudio = exclude_current_process ? YES : NO;
        config.sampleRate = (NSInteger)sample_rate;
        config.channelCount = (NSInteger)channels;
        config.width = 2;
        config.height = 2;
        config.minimumFrameInterval = CMTimeMake(1, 1);

        RsRelaySckOutputDelegate *delegate = [[RsRelaySckOutputDelegate alloc] init];
        delegate.callback = callback;
        delegate.userCtx = user_ctx;

        SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:delegate];

        dispatch_queue_attr_t qosAttr = dispatch_queue_attr_make_with_qos_class(
            DISPATCH_QUEUE_SERIAL,
            QOS_CLASS_USER_INTERACTIVE,
            0
        );
        dispatch_queue_t audioQueue = dispatch_queue_create("com.rsrelay.sck.audio", qosAttr);

        NSError *addError = nil;
        BOOL added = [stream addStreamOutput:delegate
                                        type:SCStreamOutputTypeAudio
                          sampleHandlerQueue:audioQueue
                                       error:&addError];
        if (!added || addError != nil) {
            return NULL;
        }

        __block NSError *startError = nil;
        dispatch_semaphore_t startSem = dispatch_semaphore_create(0);
        [stream startCaptureWithCompletionHandler:^(NSError * _Nullable error) {
            startError = error;
            dispatch_semaphore_signal(startSem);
        }];

        if (dispatch_semaphore_wait(startSem, timeout) != 0 || startError != nil) {
            return NULL;
        }

        RsRelaySckSession *session = [[RsRelaySckSession alloc] init];
        session.stream = stream;
        session.delegate = delegate;
        session.audioQueue = audioQueue;

        return (__bridge_retained void *)session;
    }
    return NULL;
}

void rsrelay_sck_stream_stop(void *session_ptr) {
    if (session_ptr == NULL) {
        return;
    }
    if (@available(macOS 13.0, *)) {
        RsRelaySckSession *session = (__bridge_transfer RsRelaySckSession *)session_ptr;
        session.delegate.callback = NULL;
        session.delegate.userCtx = NULL;

        dispatch_semaphore_t stopSem = dispatch_semaphore_create(0);
        [session.stream stopCaptureWithCompletionHandler:^(NSError * _Nullable error) {
            (void)error;
            dispatch_semaphore_signal(stopSem);
        }];
        dispatch_time_t timeout = dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1.5 * NSEC_PER_SEC));
        dispatch_semaphore_wait(stopSem, timeout);
    }
}
