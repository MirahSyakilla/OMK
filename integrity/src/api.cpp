#include "payload.h"
#include "zygisk.hpp"

#include <android/log.h>
#include <jni.h>
#include <string.h>
#include <sys/system_properties.h>
#include <unistd.h>

#include <string>
#include <string_view>

#define LOGD(...) __android_log_print(ANDROID_LOG_DEBUG, "OMK-Integrity", __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, "OMK-Integrity", __VA_ARGS__)

extern "C" int omk_integrity_companion(int fd);
extern "C" int omk_integrity_recv(int fd, omk_integrity_payload *out);

namespace {

constexpr const char *DROIDGUARD_PACKAGE = "com.google.android.gms.unstable";
constexpr const char *VENDING_PACKAGE = "com.android.vending";

omk_integrity_payload gPayload{};
JNIEnv *gEnv = nullptr;

using T_Callback = void (*)(void *, const char *, const char *, uint32_t);
void (*o_system_property_read_callback)(const prop_info *, T_Callback, void *) = nullptr;
thread_local T_Callback o_callback = nullptr;

bool ends_with(std::string_view value, std::string_view suffix) {
    return value.size() >= suffix.size() &&
           value.compare(value.size() - suffix.size(), suffix.size(), suffix) == 0;
}

const char *field_or_null(const char *value) {
    return value && value[0] ? value : nullptr;
}

void modifyCallback(void *cookie, const char *name, const char *value, uint32_t serial) {
    if (!cookie || !name || !value || !o_callback) {
        return;
    }

    const char *oldValue = value;
    const std::string_view prop(name);

    if (prop == "init.svc.adbd") {
        value = "stopped";
    } else if (prop == "sys.usb.state") {
        value = "mtp";
    } else if (ends_with(prop, "api_level")) {
        if (const char *next = field_or_null(gPayload.initial_sdk)) {
            value = next;
        }
    } else if (ends_with(prop, ".security_patch")) {
        if (const char *next = field_or_null(gPayload.security_patch)) {
            value = next;
        }
    } else if (ends_with(prop, ".build.id")) {
        if (const char *next = field_or_null(gPayload.id)) {
            value = next;
        }
    } else if (prop == "ro.build.fingerprint" || prop == "ro.vendor.build.fingerprint") {
        if (const char *next = field_or_null(gPayload.fingerprint)) {
            value = next;
        }
    } else if (prop == "ro.product.brand" || prop == "ro.product.system.brand") {
        if (const char *next = field_or_null(gPayload.brand)) {
            value = next;
        }
    } else if (prop == "ro.product.device" || prop == "ro.product.system.device") {
        if (const char *next = field_or_null(gPayload.device)) {
            value = next;
        }
    } else if (prop == "ro.product.model" || prop == "ro.product.system.model") {
        if (const char *next = field_or_null(gPayload.model)) {
            value = next;
        }
    } else if (prop == "ro.product.manufacturer" || prop == "ro.product.system.manufacturer") {
        if (const char *next = field_or_null(gPayload.manufacturer)) {
            value = next;
        }
    } else if (prop == "ro.product.name" || prop == "ro.product.system.name") {
        if (const char *next = field_or_null(gPayload.product)) {
            value = next;
        }
    }

    if (strcmp(oldValue, value) != 0) {
        LOGD("[%s]: %s -> %s", name, oldValue, value);
    }
    o_callback(cookie, name, value, serial);
}

void hookedPropertyReadCallback(const prop_info *pi, T_Callback callback, void *cookie) {
    if (pi && callback && cookie) {
        o_callback = callback;
    }
    o_system_property_read_callback(pi, modifyCallback, cookie);
}

void setStringField(jclass cls, const char *name, const char *value) {
    if (!gEnv || !cls || !value || !value[0]) {
        return;
    }
    jfieldID field = gEnv->GetStaticFieldID(cls, name, "Ljava/lang/String;");
    if (gEnv->ExceptionCheck()) {
        gEnv->ExceptionClear();
        return;
    }
    jstring jValue = gEnv->NewStringUTF(value);
    gEnv->SetStaticObjectField(cls, field, jValue);
    if (gEnv->ExceptionCheck()) {
        gEnv->ExceptionClear();
    } else {
        LOGD("Set '%s' to '%s'", name, value);
    }
    gEnv->DeleteLocalRef(jValue);
}

void updateBuildFields() {
    if (!gEnv) {
        return;
    }
    jclass buildClass = gEnv->FindClass("android/os/Build");
    jclass versionClass = gEnv->FindClass("android/os/Build$VERSION");
    if (!buildClass || !versionClass) {
        gEnv->ExceptionClear();
        return;
    }

    setStringField(buildClass, "FINGERPRINT", gPayload.fingerprint);
    setStringField(buildClass, "BRAND", gPayload.brand);
    setStringField(buildClass, "PRODUCT", gPayload.product);
    setStringField(buildClass, "DEVICE", gPayload.device);
    setStringField(buildClass, "MODEL", gPayload.model);
    setStringField(buildClass, "MANUFACTURER", gPayload.manufacturer);
    setStringField(buildClass, "ID", gPayload.id);
    setStringField(buildClass, "TYPE", gPayload.type_);
    setStringField(buildClass, "TAGS", gPayload.tags);
    setStringField(versionClass, "RELEASE", gPayload.release);
    setStringField(versionClass, "INCREMENTAL", gPayload.incremental);
    setStringField(versionClass, "SECURITY_PATCH", gPayload.security_patch);

    gEnv->DeleteLocalRef(versionClass);
    gEnv->DeleteLocalRef(buildClass);
}

}  // namespace

static void companion(int fd) {
    if (omk_integrity_companion(fd) != 0) {
        LOGE("companion failed to send payload");
    }
}

using namespace zygisk;

class OmKIntegrity : public ModuleBase {
public:
    void onLoad(Api *api_, JNIEnv *env_) override {
        api = api_;
        env = env_;
    }

    void preAppSpecialize(AppSpecializeArgs *args) override {
        payloadLoaded = false;
        isGmsUnstable = false;
        isVending = false;
        memset(&gPayload, 0, sizeof(gPayload));

        if (!args) {
            api->setOption(DLCLOSE_MODULE_LIBRARY);
            return;
        }

        std::string dir;
        std::string name;
        const char *rawDir = env->GetStringUTFChars(args->app_data_dir, nullptr);
        if (rawDir) {
            dir = rawDir;
            env->ReleaseStringUTFChars(args->app_data_dir, rawDir);
        }
        const char *rawName = env->GetStringUTFChars(args->nice_name, nullptr);
        if (rawName) {
            name = rawName;
            env->ReleaseStringUTFChars(args->nice_name, rawName);
        }

        const bool isGms = ends_with(dir, "/com.google.android.gms") ||
                           ends_with(dir, "/com.android.vending");
        if (!isGms) {
            api->setOption(DLCLOSE_MODULE_LIBRARY);
            return;
        }

        api->setOption(FORCE_DENYLIST_UNMOUNT);
        isGmsUnstable = name == DROIDGUARD_PACKAGE;
        isVending = name == VENDING_PACKAGE;
        if (!isGmsUnstable && !isVending) {
            api->setOption(DLCLOSE_MODULE_LIBRARY);
            return;
        }

        const int fd = api->connectCompanion();
        if (fd < 0 || omk_integrity_recv(fd, &gPayload) != 0) {
            if (fd >= 0) {
                close(fd);
            }
            api->setOption(DLCLOSE_MODULE_LIBRARY);
            return;
        }
        close(fd);

        if (!gPayload.enabled || !gPayload.fingerprint[0]) {
            api->setOption(DLCLOSE_MODULE_LIBRARY);
            return;
        }

        payloadLoaded = true;
        if (isGmsUnstable && gPayload.spoof_props) {
            api->pltHookRegister(".*", "__system_property_read_callback",
                                 reinterpret_cast<void *>(hookedPropertyReadCallback),
                                 reinterpret_cast<void **>(&o_system_property_read_callback));
            if (!api->pltHookCommit() || !o_system_property_read_callback) {
                LOGE("plt hook commit failed");
                api->setOption(DLCLOSE_MODULE_LIBRARY);
                payloadLoaded = false;
                return;
            }
        } else {
            api->setOption(DLCLOSE_MODULE_LIBRARY);
        }
    }

    void postAppSpecialize(const AppSpecializeArgs *args) override {
        (void)args;
        if (!payloadLoaded) {
            return;
        }
        gEnv = env;
        if (isGmsUnstable && gPayload.spoof_build) {
            updateBuildFields();
        } else if (isVending && gPayload.spoof_vending) {
            updateBuildFields();
        }
    }

    void preServerSpecialize(ServerSpecializeArgs *args) override {
        (void)args;
        api->setOption(DLCLOSE_MODULE_LIBRARY);
    }

private:
    Api *api = nullptr;
    JNIEnv *env = nullptr;
    bool payloadLoaded = false;
    bool isGmsUnstable = false;
    bool isVending = false;
};

extern "C" {

void omk_zygisk_module_entry(zygisk::internal::api_table *table, JNIEnv *env) {
    zygisk::internal::entry_impl<OmKIntegrity>(table, env);
}

void omk_zygisk_companion_entry(int client) {
    companion(client);
}

}
