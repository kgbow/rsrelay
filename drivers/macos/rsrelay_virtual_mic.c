// rsrelay_virtual_mic.c
//
// Zero-dependency macOS CoreAudio AudioServerPlugIn (`rsrelayVirtualMic.driver`)
// implementing a dedicated split Input/Sink virtual microphone architecture:
//
//   1. "rsrelay Virtual Mic"      (UID: dev.rsrelay.virtualmic.input)
//      Pure Capture/Input endpoint ONLY. Appears in Zoom, Chrome, WeChat, Discord, OBS,
//      and System Settings > Sound > Input. Has 0 Output streams so it NEVER pollutes
//      speaker selection menus.
//
//   2. "rsrelay Virtual Mic Sink" (UID: dev.rsrelay.virtualmic.sink)
//      Pure Render/Output bridge endpoint with `CanBeDefaultDevice = 0` so macOS NEVER
//      auto-switches system speakers to it. `rsrelay` feeds 48 kHz stereo Float32 voice
//      frames here via CoreAudio hardware-clocked callbacks, bridging lock-free in-memory
//      straight to "rsrelay Virtual Mic" inside `coreaudiod` / `DriverHelper` with zero
//      Seatbelt sandbox IPC issues on macOS Ventura / Sonoma / Sequoia.

#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#define kPlugIn_BundleID "dev.rsrelay.driver.virtualmic"
#define kManufacturer_Name "rsrelay Audio"

#define kMic_DeviceName "rsrelay Virtual Mic"
#define kMic_DeviceUID "dev.rsrelay.virtualmic.input"
#define kMic_ModelUID "dev.rsrelay.virtualmic.model.input"

#define kSink_DeviceName "rsrelay Virtual Mic Sink"
#define kSink_DeviceUID "dev.rsrelay.virtualmic.sink"
#define kSink_ModelUID "dev.rsrelay.virtualmic.model.sink"

enum {
    kObjectID_PlugIn            = kAudioObjectPlugInObject, // 1
    kObjectID_Device_Mic        = 2,
    kObjectID_Stream_MicInput   = 3,
    kObjectID_Device_Sink       = 4,
    kObjectID_Stream_SinkOutput = 5,
};

#define kChannel_Count 2
#define kDefault_SampleRate 48000.0
#define kZeroTimeStamp_PeriodFrames 2048
#define kRing_Frames 16384u
#define kRing_Mask (kRing_Frames - 1u)
#define kMax_Latency_Frames 1920u // 40 ms @ 48 kHz

static pthread_mutex_t gStateMutex = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gPlugInRefCount = 0;
static AudioServerPlugInHostRef gPlugInHost = NULL;

static Float64 gSampleRate = kDefault_SampleRate;
static Float64 gHostTicksPerFrame = 0.0;

typedef struct {
    atomic_uint io_client_count;
    UInt64 anchor_host_time;
    UInt64 number_time_stamps;
    UInt64 timeline_seed;
} DeviceClockState;

static DeviceClockState gMicClock = {0, 0, 0, 1};
static DeviceClockState gSinkClock = {0, 0, 0, 1};

// Lock-free SPSC stereo Float32 bridge ring buffer shared inside DriverHelper process space
static float gBridgeRing[kRing_Frames * kChannel_Count];
static atomic_uint_fast64_t gWriteFrameIdx = 0;
static atomic_uint_fast64_t gReadFrameIdx = 0;

static void UpdateHostTicksPerFrame(void) {
    mach_timebase_info_data_t tb;
    mach_timebase_info(&tb);
    Float64 hostClockFreq = (1.0e9 * (Float64)tb.denom) / (Float64)tb.numer;
    gHostTicksPerFrame = hostClockFreq / gSampleRate;
}

static DeviceClockState* GetClockForDevice(AudioObjectID devID) {
    if (devID == kObjectID_Device_Mic) return &gMicClock;
    if (devID == kObjectID_Device_Sink) return &gSinkClock;
    return NULL;
}

static AudioStreamBasicDescription MakeCanonicalFormat(Float64 rate) {
    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = rate;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kAudioFormatFlagsNativeFloatPacked;
    fmt.mBytesPerPacket = kChannel_Count * sizeof(Float32);
    fmt.mFramesPerPacket = 1;
    fmt.mBytesPerFrame = kChannel_Count * sizeof(Float32);
    fmt.mChannelsPerFrame = kChannel_Count;
    fmt.mBitsPerChannel = 32;
    return fmt;
}

// Forward declarations
static HRESULT RsRelay_QueryInterface(void* inDriver, REFIID inUUID, LPVOID* outInterface);
static ULONG RsRelay_AddRef(void* inDriver);
static ULONG RsRelay_Release(void* inDriver);
static OSStatus RsRelay_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost);
static OSStatus RsRelay_CreateDevice(AudioServerPlugInDriverRef inDriver, CFDictionaryRef inDescription, const AudioServerPlugInClientInfo* inClientInfo, AudioObjectID* outDeviceObjectID);
static OSStatus RsRelay_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID);
static OSStatus RsRelay_AddDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, const AudioServerPlugInClientInfo* inClientInfo);
static OSStatus RsRelay_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, const AudioServerPlugInClientInfo* inClientInfo);
static OSStatus RsRelay_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt64 inChangeAction, void* inChangeInfo);
static OSStatus RsRelay_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt64 inChangeAction, void* inChangeInfo);
static Boolean RsRelay_HasProperty(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress);
static OSStatus RsRelay_IsPropertySettable(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, Boolean* outIsSettable);
static OSStatus RsRelay_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32* outDataSize);
static OSStatus RsRelay_GetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32 inDataSize, UInt32* outDataSize, void* outData);
static OSStatus RsRelay_SetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32 inDataSize, const void* inData);
static OSStatus RsRelay_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus RsRelay_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus RsRelay_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, Float64* outSampleTime, UInt64* outHostTime, UInt64* outSeed);
static OSStatus RsRelay_WillDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, Boolean* outWillDo, Boolean* outWillDoInPlace);
static OSStatus RsRelay_BeginIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo);
static OSStatus RsRelay_DoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, AudioObjectID inStreamObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo, void* ioMainBuffer, void* ioSecondaryBuffer);
static OSStatus RsRelay_EndIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo);

static AudioServerPlugInDriverInterface gDriverInterface = {
    NULL,
    RsRelay_QueryInterface,
    RsRelay_AddRef,
    RsRelay_Release,
    RsRelay_Initialize,
    RsRelay_CreateDevice,
    RsRelay_DestroyDevice,
    RsRelay_AddDeviceClient,
    RsRelay_RemoveDeviceClient,
    RsRelay_PerformDeviceConfigurationChange,
    RsRelay_AbortDeviceConfigurationChange,
    RsRelay_HasProperty,
    RsRelay_IsPropertySettable,
    RsRelay_GetPropertyDataSize,
    RsRelay_GetPropertyData,
    RsRelay_SetPropertyData,
    RsRelay_StartIO,
    RsRelay_StopIO,
    RsRelay_GetZeroTimeStamp,
    RsRelay_WillDoIOOperation,
    RsRelay_BeginIOOperation,
    RsRelay_DoIOOperation,
    RsRelay_EndIOOperation
};

static AudioServerPlugInDriverInterface* gDriverInterfacePtr = &gDriverInterface;
static AudioServerPlugInDriverRef gDriverRef = &gDriverInterfacePtr;

__attribute__((visibility("default")))
void* RsRelayVirtualMic_Create(CFAllocatorRef inAllocator, CFUUIDRef inRequestedTypeUUID) {
    (void)inAllocator;
    if (CFEqual(inRequestedTypeUUID, kAudioServerPlugInTypeUUID)) {
        RsRelay_AddRef(gDriverRef);
        return gDriverRef;
    }
    return NULL;
}

static HRESULT RsRelay_QueryInterface(void* inDriver, REFIID inUUID, LPVOID* outInterface) {
    if (inDriver != gDriverRef || outInterface == NULL) {
        return E_POINTER;
    }
    CFUUIDRef req = CFUUIDCreateFromUUIDBytes(NULL, inUUID);
    bool match = CFEqual(req, IUnknownUUID) || CFEqual(req, kAudioServerPlugInDriverInterfaceUUID);
    CFRelease(req);
    if (match) {
        RsRelay_AddRef(gDriverRef);
        *outInterface = gDriverRef;
        return S_OK;
    }
    *outInterface = NULL;
    return E_NOINTERFACE;
}

static ULONG RsRelay_AddRef(void* inDriver) {
    if (inDriver != gDriverRef) return 0;
    pthread_mutex_lock(&gStateMutex);
    ++gPlugInRefCount;
    ULONG rc = gPlugInRefCount;
    pthread_mutex_unlock(&gStateMutex);
    return rc;
}

static ULONG RsRelay_Release(void* inDriver) {
    if (inDriver != gDriverRef) return 0;
    pthread_mutex_lock(&gStateMutex);
    if (gPlugInRefCount > 0) --gPlugInRefCount;
    ULONG rc = gPlugInRefCount;
    pthread_mutex_unlock(&gStateMutex);
    return rc;
}

static OSStatus RsRelay_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost) {
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gStateMutex);
    gPlugInHost = inHost;
    memset(gBridgeRing, 0, sizeof(gBridgeRing));
    UpdateHostTicksPerFrame();
    pthread_mutex_unlock(&gStateMutex);
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_CreateDevice(AudioServerPlugInDriverRef inDriver, CFDictionaryRef inDescription, const AudioServerPlugInClientInfo* inClientInfo, AudioObjectID* outDeviceObjectID) {
    (void)inDriver; (void)inDescription; (void)inClientInfo; (void)outDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus RsRelay_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID) {
    (void)inDriver; (void)inDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus RsRelay_AddDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, const AudioServerPlugInClientInfo* inClientInfo) {
    (void)inDriver; (void)inClientInfo;
    if (inDeviceObjectID != kObjectID_Device_Mic && inDeviceObjectID != kObjectID_Device_Sink) {
        return kAudioHardwareBadObjectError;
    }
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, const AudioServerPlugInClientInfo* inClientInfo) {
    (void)inDriver; (void)inClientInfo;
    if (inDeviceObjectID != kObjectID_Device_Mic && inDeviceObjectID != kObjectID_Device_Sink) {
        return kAudioHardwareBadObjectError;
    }
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt64 inChangeAction, void* inChangeInfo) {
    (void)inDriver; (void)inDeviceObjectID; (void)inChangeAction; (void)inChangeInfo;
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt64 inChangeAction, void* inChangeInfo) {
    (void)inDriver; (void)inDeviceObjectID; (void)inChangeAction; (void)inChangeInfo;
    return kAudioHardwareNoError;
}

static Boolean RsRelay_HasProperty(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || inAddress == NULL) return false;

    switch (inObjectID) {
        case kObjectID_PlugIn:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyManufacturer:
                case kAudioObjectPropertyOwnedObjects:
                case kAudioPlugInPropertyDeviceList:
                case kAudioPlugInPropertyTranslateUIDToDevice:
                case kAudioPlugInPropertyResourceBundle:
                    return true;
                default:
                    return false;
            }

        case kObjectID_Device_Mic:
        case kObjectID_Device_Sink:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyName:
                case kAudioObjectPropertyManufacturer:
                case kAudioObjectPropertyOwnedObjects:
                case kAudioDevicePropertyDeviceUID:
                case kAudioDevicePropertyModelUID:
                case kAudioDevicePropertyTransportType:
                case kAudioDevicePropertyRelatedDevices:
                case kAudioDevicePropertyClockDomain:
                case kAudioDevicePropertyDeviceIsAlive:
                case kAudioDevicePropertyDeviceIsRunning:
                case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
                case kAudioDevicePropertyLatency:
                case kAudioDevicePropertyStreams:
                case kAudioObjectPropertyControlList:
                case kAudioDevicePropertySafetyOffset:
                case kAudioDevicePropertyNominalSampleRate:
                case kAudioDevicePropertyAvailableNominalSampleRates:
                case kAudioDevicePropertyIsHidden:
                case kAudioDevicePropertyZeroTimeStampPeriod:
                    return true;
                default:
                    return false;
            }

        case kObjectID_Stream_MicInput:
        case kObjectID_Stream_SinkOutput:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyOwnedObjects:
                case kAudioStreamPropertyIsActive:
                case kAudioStreamPropertyDirection:
                case kAudioStreamPropertyTerminalType:
                case kAudioStreamPropertyStartingChannel:
                case kAudioStreamPropertyLatency:
                case kAudioStreamPropertyVirtualFormat:
                case kAudioStreamPropertyPhysicalFormat:
                case kAudioStreamPropertyAvailableVirtualFormats:
                case kAudioStreamPropertyAvailablePhysicalFormats:
                    return true;
                default:
                    return false;
            }
        default:
            return false;
    }
}

static OSStatus RsRelay_IsPropertySettable(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, Boolean* outIsSettable) {
    if (!RsRelay_HasProperty(inDriver, inObjectID, inClientProcessID, inAddress)) {
        return kAudioHardwareUnknownPropertyError;
    }
    *outIsSettable = false;
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32* outDataSize) {
    (void)inQualifierDataSize; (void)inQualifierData;
    if (!RsRelay_HasProperty(inDriver, inObjectID, inClientProcessID, inAddress)) {
        return kAudioHardwareUnknownPropertyError;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                case kAudioPlugInPropertyTranslateUIDToDevice:
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyManufacturer:
                case kAudioPlugInPropertyResourceBundle:
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                case kAudioPlugInPropertyDeviceList:
                    *outDataSize = 2 * sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                default:
                    return kAudioHardwareUnknownPropertyError;
            }

        case kObjectID_Device_Mic:
        case kObjectID_Device_Sink: {
            bool isMic = (inObjectID == kObjectID_Device_Mic);
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyName:
                case kAudioObjectPropertyManufacturer:
                case kAudioDevicePropertyDeviceUID:
                case kAudioDevicePropertyModelUID:
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyTransportType:
                case kAudioDevicePropertyClockDomain:
                case kAudioDevicePropertyDeviceIsAlive:
                case kAudioDevicePropertyDeviceIsRunning:
                case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
                case kAudioDevicePropertyLatency:
                case kAudioDevicePropertySafetyOffset:
                case kAudioDevicePropertyIsHidden:
                case kAudioDevicePropertyZeroTimeStampPeriod:
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyRelatedDevices:
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                    if (inAddress->mScope == kAudioObjectPropertyScopeGlobal
                        || (isMic && inAddress->mScope == kAudioObjectPropertyScopeInput)
                        || (!isMic && inAddress->mScope == kAudioObjectPropertyScopeOutput)) {
                        *outDataSize = sizeof(AudioObjectID);
                    } else {
                        *outDataSize = 0;
                    }
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyStreams:
                    if ((isMic && (inAddress->mScope == kAudioObjectPropertyScopeGlobal || inAddress->mScope == kAudioObjectPropertyScopeInput))
                        || (!isMic && (inAddress->mScope == kAudioObjectPropertyScopeGlobal || inAddress->mScope == kAudioObjectPropertyScopeOutput))) {
                        *outDataSize = sizeof(AudioObjectID);
                    } else {
                        *outDataSize = 0;
                    }
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyControlList:
                    *outDataSize = 0;
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyNominalSampleRate:
                    *outDataSize = sizeof(Float64);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyAvailableNominalSampleRates:
                    *outDataSize = sizeof(AudioValueRange);
                    return kAudioHardwareNoError;
                default:
                    return kAudioHardwareUnknownPropertyError;
            }
        }

        case kObjectID_Stream_MicInput:
        case kObjectID_Stream_SinkOutput:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                    *outDataSize = 0;
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyIsActive:
                case kAudioStreamPropertyDirection:
                case kAudioStreamPropertyTerminalType:
                case kAudioStreamPropertyStartingChannel:
                case kAudioStreamPropertyLatency:
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyVirtualFormat:
                case kAudioStreamPropertyPhysicalFormat:
                    *outDataSize = sizeof(AudioStreamBasicDescription);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyAvailableVirtualFormats:
                case kAudioStreamPropertyAvailablePhysicalFormats:
                    *outDataSize = sizeof(AudioStreamRangedDescription);
                    return kAudioHardwareNoError;
                default:
                    return kAudioHardwareUnknownPropertyError;
            }
        default:
            return kAudioHardwareBadObjectError;
    }
}

static OSStatus RsRelay_GetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32 inDataSize, UInt32* outDataSize, void* outData) {
    if (!RsRelay_HasProperty(inDriver, inObjectID, inClientProcessID, inAddress)) {
        return kAudioHardwareUnknownPropertyError;
    }

    switch (inObjectID) {
        case kObjectID_PlugIn:
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioObjectClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioPlugInClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                    if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioObjectID*)outData) = kAudioObjectUnknown;
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyManufacturer:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = CFSTR(kManufacturer_Name);
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioPlugInPropertyResourceBundle:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = CFSTR("");
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                case kAudioPlugInPropertyDeviceList: {
                    UInt32 count = inDataSize / sizeof(AudioObjectID);
                    AudioObjectID* list = (AudioObjectID*)outData;
                    if (count >= 1) list[0] = kObjectID_Device_Mic;
                    if (count >= 2) list[1] = kObjectID_Device_Sink;
                    *outDataSize = (count >= 2 ? 2 : count) * sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                }
                case kAudioPlugInPropertyTranslateUIDToDevice: {
                    if (inQualifierDataSize < sizeof(CFStringRef) || inQualifierData == NULL) {
                        return kAudioHardwareBadPropertySizeError;
                    }
                    if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                    CFStringRef uid = *((const CFStringRef*)inQualifierData);
                    if (CFEqual(uid, CFSTR(kMic_DeviceUID))) {
                        *((AudioObjectID*)outData) = kObjectID_Device_Mic;
                    } else if (CFEqual(uid, CFSTR(kSink_DeviceUID))) {
                        *((AudioObjectID*)outData) = kObjectID_Device_Sink;
                    } else {
                        *((AudioObjectID*)outData) = kAudioObjectUnknown;
                    }
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                }
                default:
                    return kAudioHardwareUnknownPropertyError;
            }

        case kObjectID_Device_Mic:
        case kObjectID_Device_Sink: {
            bool isMic = (inObjectID == kObjectID_Device_Mic);
            DeviceClockState* clk = GetClockForDevice(inObjectID);
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioObjectClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioDeviceClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                    if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioObjectID*)outData) = kObjectID_PlugIn;
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyName:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = isMic ? CFSTR(kMic_DeviceName) : CFSTR(kSink_DeviceName);
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyManufacturer:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = CFSTR(kManufacturer_Name);
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyDeviceUID:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = isMic ? CFSTR(kMic_DeviceUID) : CFSTR(kSink_DeviceUID);
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyModelUID:
                    if (inDataSize < sizeof(CFStringRef)) return kAudioHardwareBadPropertySizeError;
                    *((CFStringRef*)outData) = isMic ? CFSTR(kMic_ModelUID) : CFSTR(kSink_ModelUID);
                    *outDataSize = sizeof(CFStringRef);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyTransportType:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = kAudioDeviceTransportTypeVirtual;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyRelatedDevices:
                    if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioObjectID*)outData) = inObjectID;
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyClockDomain:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 0x5253524Cu; // 'RSRL' shared clock domain
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyDeviceIsAlive:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 1;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyDeviceIsRunning:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = (atomic_load(&clk->io_client_count) > 0) ? 1 : 0;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    // Mic device can be selected as Default Input; Sink NEVER steals Default Speaker!
                    if (isMic && inAddress->mScope == kAudioObjectPropertyScopeInput) {
                        *((UInt32*)outData) = 1;
                    } else {
                        *((UInt32*)outData) = 0;
                    }
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 0;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyLatency:
                case kAudioDevicePropertySafetyOffset:
                case kAudioDevicePropertyIsHidden:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 0;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyZeroTimeStampPeriod:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = kZeroTimeStamp_PeriodFrames;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                case kAudioDevicePropertyStreams:
                    if ((isMic && (inAddress->mScope == kAudioObjectPropertyScopeGlobal || inAddress->mScope == kAudioObjectPropertyScopeInput))
                        || (!isMic && (inAddress->mScope == kAudioObjectPropertyScopeGlobal || inAddress->mScope == kAudioObjectPropertyScopeOutput))) {
                        if (inDataSize < sizeof(AudioObjectID)) {
                            *outDataSize = 0;
                            return kAudioHardwareNoError;
                        }
                        *((AudioObjectID*)outData) = isMic ? kObjectID_Stream_MicInput : kObjectID_Stream_SinkOutput;
                        *outDataSize = sizeof(AudioObjectID);
                    } else {
                        *outDataSize = 0;
                    }
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyControlList:
                    *outDataSize = 0;
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyNominalSampleRate:
                    if (inDataSize < sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
                    *((Float64*)outData) = gSampleRate;
                    *outDataSize = sizeof(Float64);
                    return kAudioHardwareNoError;
                case kAudioDevicePropertyAvailableNominalSampleRates: {
                    if (inDataSize < sizeof(AudioValueRange)) return kAudioHardwareBadPropertySizeError;
                    AudioValueRange* rng = (AudioValueRange*)outData;
                    rng->mMinimum = kDefault_SampleRate;
                    rng->mMaximum = kDefault_SampleRate;
                    *outDataSize = sizeof(AudioValueRange);
                    return kAudioHardwareNoError;
                }
                default:
                    return kAudioHardwareUnknownPropertyError;
            }
        }

        case kObjectID_Stream_MicInput:
        case kObjectID_Stream_SinkOutput: {
            bool isInputStream = (inObjectID == kObjectID_Stream_MicInput);
            switch (inAddress->mSelector) {
                case kAudioObjectPropertyBaseClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioObjectClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyClass:
                    if (inDataSize < sizeof(AudioClassID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioClassID*)outData) = kAudioStreamClassID;
                    *outDataSize = sizeof(AudioClassID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwner:
                    if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                    *((AudioObjectID*)outData) = isInputStream ? kObjectID_Device_Mic : kObjectID_Device_Sink;
                    *outDataSize = sizeof(AudioObjectID);
                    return kAudioHardwareNoError;
                case kAudioObjectPropertyOwnedObjects:
                    *outDataSize = 0;
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyIsActive:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 1;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyDirection:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    // CoreAudio convention: 1 = Input stream, 0 = Output stream
                    *((UInt32*)outData) = isInputStream ? 1 : 0;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyTerminalType:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = isInputStream ? kAudioStreamTerminalTypeMicrophone : kAudioStreamTerminalTypeSpeaker;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyStartingChannel:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 1;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyLatency:
                    if (inDataSize < sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
                    *((UInt32*)outData) = 0;
                    *outDataSize = sizeof(UInt32);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyVirtualFormat:
                case kAudioStreamPropertyPhysicalFormat:
                    if (inDataSize < sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
                    *((AudioStreamBasicDescription*)outData) = MakeCanonicalFormat(gSampleRate);
                    *outDataSize = sizeof(AudioStreamBasicDescription);
                    return kAudioHardwareNoError;
                case kAudioStreamPropertyAvailableVirtualFormats:
                case kAudioStreamPropertyAvailablePhysicalFormats: {
                    if (inDataSize < sizeof(AudioStreamRangedDescription)) return kAudioHardwareBadPropertySizeError;
                    AudioStreamRangedDescription* rdesc = (AudioStreamRangedDescription*)outData;
                    rdesc->mFormat = MakeCanonicalFormat(gSampleRate);
                    rdesc->mSampleRateRange.mMinimum = kDefault_SampleRate;
                    rdesc->mSampleRateRange.mMaximum = kDefault_SampleRate;
                    *outDataSize = sizeof(AudioStreamRangedDescription);
                    return kAudioHardwareNoError;
                }
                default:
                    return kAudioHardwareUnknownPropertyError;
            }
        }
        default:
            return kAudioHardwareBadObjectError;
    }
}

static OSStatus RsRelay_SetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID, const AudioObjectPropertyAddress* inAddress, UInt32 inQualifierDataSize, const void* inQualifierData, UInt32 inDataSize, const void* inData) {
    (void)inDriver; (void)inObjectID; (void)inClientProcessID; (void)inAddress;
    (void)inQualifierDataSize; (void)inQualifierData; (void)inDataSize; (void)inData;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus RsRelay_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    DeviceClockState* clk = GetClockForDevice(inDeviceObjectID);
    if (clk == NULL) return kAudioHardwareBadObjectError;

    pthread_mutex_lock(&gStateMutex);
    unsigned int prev = atomic_fetch_add(&clk->io_client_count, 1);
    if (prev == 0) {
        clk->number_time_stamps = 0;
        clk->timeline_seed += 1;
        clk->anchor_host_time = mach_absolute_time();
        if (inDeviceObjectID == kObjectID_Device_Sink) {
            uint64_t w = atomic_load(&gWriteFrameIdx);
            atomic_store(&gReadFrameIdx, w);
        }
    }
    pthread_mutex_unlock(&gStateMutex);
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    DeviceClockState* clk = GetClockForDevice(inDeviceObjectID);
    if (clk == NULL) return kAudioHardwareBadObjectError;

    pthread_mutex_lock(&gStateMutex);
    unsigned int prev = atomic_load(&clk->io_client_count);
    if (prev > 0) {
        atomic_fetch_sub(&clk->io_client_count, 1);
        if (prev == 1 && inDeviceObjectID == kObjectID_Device_Sink) {
            memset(gBridgeRing, 0, sizeof(gBridgeRing));
        }
    }
    pthread_mutex_unlock(&gStateMutex);
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, Float64* outSampleTime, UInt64* outHostTime, UInt64* outSeed) {
    (void)inClientID;
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    DeviceClockState* clk = GetClockForDevice(inDeviceObjectID);
    if (clk == NULL) return kAudioHardwareBadObjectError;

    UInt64 now = mach_absolute_time();
    Float64 ticksPerPeriod = gHostTicksPerFrame * (Float64)kZeroTimeStamp_PeriodFrames;
    UInt64 periodTicks = (UInt64)ticksPerPeriod;
    if (periodTicks == 0) periodTicks = 1;

    UInt64 nextHost = clk->anchor_host_time + (UInt64)((Float64)(clk->number_time_stamps + 1) * ticksPerPeriod);
    if (now >= nextHost) {
        UInt64 elapsedPeriods = (now - clk->anchor_host_time) / periodTicks;
        clk->number_time_stamps = elapsedPeriods;
    }

    *outSampleTime = (Float64)(clk->number_time_stamps * kZeroTimeStamp_PeriodFrames);
    *outHostTime = clk->anchor_host_time + (UInt64)((Float64)clk->number_time_stamps * ticksPerPeriod);
    *outSeed = clk->timeline_seed;
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_WillDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, Boolean* outWillDo, Boolean* outWillDoInPlace) {
    (void)inClientID;
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    bool willDo = false;
    if (inDeviceObjectID == kObjectID_Device_Mic && inOperationID == kAudioServerPlugInIOOperationReadInput) {
        willDo = true;
    } else if (inDeviceObjectID == kObjectID_Device_Sink && inOperationID == kAudioServerPlugInIOOperationWriteMix) {
        willDo = true;
    }
    *outWillDo = willDo;
    *outWillDoInPlace = true;
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_BeginIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo) {
    (void)inDriver; (void)inDeviceObjectID; (void)inClientID; (void)inOperationID; (void)inIOBufferFrameSize; (void)inIOCycleInfo;
    return kAudioHardwareNoError;
}

static OSStatus RsRelay_DoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, AudioObjectID inStreamObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo, void* ioMainBuffer, void* ioSecondaryBuffer) {
    (void)inDriver; (void)inStreamObjectID; (void)inClientID; (void)inIOCycleInfo; (void)ioSecondaryBuffer;
    if (ioMainBuffer == NULL || inIOBufferFrameSize == 0) {
        return kAudioHardwareNoError;
    }

    if (inDeviceObjectID == kObjectID_Device_Sink && inOperationID == kAudioServerPlugInIOOperationWriteMix) {
        const float* src = (const float*)ioMainBuffer;
        uint64_t w = atomic_load_explicit(&gWriteFrameIdx, memory_order_relaxed);
        for (UInt32 f = 0; f < inIOBufferFrameSize; ++f) {
            uint32_t slot = (uint32_t)((w + f) & kRing_Mask) * kChannel_Count;
            gBridgeRing[slot]     = src[f * kChannel_Count];
            gBridgeRing[slot + 1] = src[f * kChannel_Count + 1];
        }
        atomic_store_explicit(&gWriteFrameIdx, w + inIOBufferFrameSize, memory_order_release);
    } else if (inDeviceObjectID == kObjectID_Device_Mic && inOperationID == kAudioServerPlugInIOOperationReadInput) {
        float* dst = (float*)ioMainBuffer;
        if (atomic_load_explicit(&gSinkClock.io_client_count, memory_order_relaxed) == 0) {
            memset(dst, 0, (size_t)inIOBufferFrameSize * kChannel_Count * sizeof(float));
            return kAudioHardwareNoError;
        }

        uint64_t w = atomic_load_explicit(&gWriteFrameIdx, memory_order_acquire);
        uint64_t r = atomic_load_explicit(&gReadFrameIdx, memory_order_relaxed);
        if (r > w || (w - r) > kMax_Latency_Frames) {
            // Re-anchor reader ~10ms (480 frames) behind writer to keep voice latency ultra-low
            r = (w > 480u) ? (w - 480u) : 0u;
        }

        uint64_t avail = (w >= r) ? (w - r) : 0u;
        UInt32 toRead = (avail < (uint64_t)inIOBufferFrameSize) ? (UInt32)avail : inIOBufferFrameSize;

        for (UInt32 f = 0; f < toRead; ++f) {
            uint32_t slot = (uint32_t)((r + f) & kRing_Mask) * kChannel_Count;
            dst[f * kChannel_Count]     = gBridgeRing[slot];
            dst[f * kChannel_Count + 1] = gBridgeRing[slot + 1];
        }
        if (toRead < inIOBufferFrameSize) {
            memset(&dst[toRead * kChannel_Count], 0, (size_t)(inIOBufferFrameSize - toRead) * kChannel_Count * sizeof(float));
        }
        atomic_store_explicit(&gReadFrameIdx, r + toRead, memory_order_relaxed);
    }

    return kAudioHardwareNoError;
}

static OSStatus RsRelay_EndIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo* inIOCycleInfo) {
    (void)inDriver; (void)inDeviceObjectID; (void)inClientID; (void)inOperationID; (void)inIOBufferFrameSize; (void)inIOCycleInfo;
    return kAudioHardwareNoError;
}
