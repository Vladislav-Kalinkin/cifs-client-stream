#ifndef CIFS_CLIENT_STREAM_H
#define CIFS_CLIENT_STREAM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

char *cifs_client_stream_bridge_version(void);

char *cifs_client_stream_smb_probe(const char *host, const char *share,
                                   const char *user, const char *password,
                                   uint64_t timeout_ms);

void cifs_client_stream_free_string(char *ptr);

#ifdef __cplusplus
}
#endif

#endif
