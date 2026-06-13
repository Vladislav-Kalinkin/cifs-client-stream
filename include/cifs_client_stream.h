#ifndef CIFS_CLIENT_STREAM_H
#define CIFS_CLIENT_STREAM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CifsClientStreamSession CifsClientStreamSession;

char *cifs_client_stream_bridge_version(void);

char *cifs_client_stream_smb_probe(const char *host, const char *share,
                                   const char *user, const char *password,
                                   uint64_t timeout_ms);

char *cifs_client_stream_smb_list(const char *host, const char *share,
                                  const char *user, const char *password,
                                  const char *path, uint64_t max_entries,
                                  uint64_t timeout_ms);

char *cifs_client_stream_smb_list_json(const char *host, const char *share,
                                       const char *user, const char *password,
                                       const char *path, uint64_t max_entries,
                                       uint64_t timeout_ms);

CifsClientStreamSession *
cifs_client_stream_session_open(const char *host, const char *share,
                                const char *user, const char *password,
                                uint64_t timeout_ms, char **out_message);

char *cifs_client_stream_session_list_json(CifsClientStreamSession *session,
                                           const char *path,
                                           uint64_t max_entries,
                                           uint64_t timeout_ms);

void cifs_client_stream_session_close(CifsClientStreamSession *session);

void cifs_client_stream_free_string(char *ptr);

#ifdef __cplusplus
}
#endif

#endif
