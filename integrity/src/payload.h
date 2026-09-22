#pragma once

#include <stdint.h>

typedef struct omk_integrity_payload {
    uint8_t enabled;
    uint8_t spoof_build;
    uint8_t spoof_props;
    uint8_t spoof_vending;
    char fingerprint[384];
    char brand[64];
    char product[64];
    char device[64];
    char model[64];
    char manufacturer[64];
    char id[64];
    char incremental[64];
    char type_[32];
    char tags[32];
    char release[32];
    char security_patch[16];
    char initial_sdk[8];
    uint8_t soter_beta;
} omk_integrity_payload;
