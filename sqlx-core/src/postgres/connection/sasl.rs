use crate::error::Error;
use crate::postgres::connection::stream::PgStream;
use crate::postgres::message::{
    Authentication, AuthenticationSasl, MessageFormat, SaslInitialResponse, SaslResponse,
};
use crate::postgres::PgConnectOptions;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::digest::Digest;
use sha2::Sha256;
use stringprep::saslprep;

const GS2_HEADER: &str = "n,,";
const CHANNEL_ATTR: &str = "c";
const USERNAME_ATTR: &str = "n";
const CLIENT_PROOF_ATTR: &str = "p";
const NONCE_ATTR: &str = "r";

pub(crate) async fn authenticate(
    stream: &mut PgStream,
    options: &PgConnectOptions,
    data: AuthenticationSasl,
) -> Result<(), Error> {
    let mut has_sasl = false;
    let mut has_sasl_plus = false;
    let mut unknown = Vec::new();

    for mechanism in data.mechanisms() {
        match mechanism {
            "SCRAM-SHA-256" => {
                has_sasl = true;
            }

            "SCRAM-SHA-256-PLUS" => {
                has_sasl_plus = true;
            }

            _ => {
                unknown.push(mechanism.to_owned());
            }
        }
    }

    if !has_sasl_plus && !has_sasl {
        return Err(err_protocol!(
            "unsupported SASL authentication mechanisms: {}",
            unknown.join(", ")
        ));
    }

    // channel-binding = "c=" base64
    let channel_binding = format!("{}={}", CHANNEL_ATTR, base64::encode(GS2_HEADER));

    // "n=" saslname ;; Usernames are prepared using SASLprep.
    let username = format!("{}={}", USERNAME_ATTR, options.username);
    let username = match saslprep(&username) {
        Ok(v) => v,
        // TODO(danielakhterov): Remove panic when we have proper support for configuration errors
        Err(_) => panic!("Failed to saslprep username"),
    };

    // nonce = "r=" c-nonce [s-nonce] ;; Second part provided by server.
    let nonce = gen_nonce();

    // client-first-message-bare = [reserved-mext ","] username "," nonce ["," extensions]
    let client_first_message_bare =
        format!("{username},{nonce}", username = username, nonce = nonce);

    let client_first_message = format!(
        "{gs2_header}{client_first_message_bare}",
        gs2_header = GS2_HEADER,
        client_first_message_bare = client_first_message_bare
    );

    stream
        .send(SaslInitialResponse {
            response: &client_first_message,
            plus: false,
        })
        .await?;

    let cont = match stream.recv_expect(MessageFormat::Authentication).await? {
        Authentication::SaslContinue(data) => data,

        auth => {
            return Err(err_protocol!(
                "expected SASLContinue but received {:?}",
                auth
            ));
        }
    };

    // SaltedPassword := Hi(Normalize(password), salt, i)
    let salted_password = hi(
        options.password.as_deref().unwrap_or_default(),
        &cont.salt,
        cont.iterations,
    )?;

    // ClientKey := HMAC(SaltedPassword, "Client Key")
    let mut mac = Hmac::<Sha256>::new_varkey(&salted_password).map_err(Error::protocol)?;
    mac.input(b"Client Key");

    let client_key = mac.result().code();

    // StoredKey := H(ClientKey)
    let stored_key = Sha256::digest(&client_key);

    // client-final-message-without-proof
    let client_final_message_wo_proof = format!(
        "{channel_binding},r={nonce}",
        channel_binding = channel_binding,
        nonce = &cont.nonce
    );

    // AuthMessage := client-first-message-bare + "," + server-first-message + "," + client-final-message-without-proof
    let auth_message = format!(
        "{client_first_message_bare},{server_first_message},{client_final_message_wo_proof}",
        client_first_message_bare = client_first_message_bare,
        server_first_message = cont.message,
        client_final_message_wo_proof = client_final_message_wo_proof
    );

    // ClientSignature := HMAC(StoredKey, AuthMessage)
    let mut mac = Hmac::<Sha256>::new_varkey(&stored_key).map_err(Error::protocol)?;
    mac.input(&auth_message.as_bytes());

    let client_signature = mac.result().code();

    // ClientProof := ClientKey XOR ClientSignature
    let client_proof: Vec<u8> = client_key
        .iter()
        .zip(client_signature.iter())
        .map(|(&a, &b)| a ^ b)
        .collect();

    // ServerKey := HMAC(SaltedPassword, "Server Key")
    let mut mac = Hmac::<Sha256>::new_varkey(&salted_password).map_err(Error::protocol)?;
    mac.input(b"Server Key");

    let server_key = mac.result().code();

    // ServerSignature := HMAC(ServerKey, AuthMessage)
    let mut mac = Hmac::<Sha256>::new_varkey(&server_key).map_err(Error::protocol)?;
    mac.input(&auth_message.as_bytes());

    // client-final-message = client-final-message-without-proof "," proof
    let client_final_message = format!(
        "{client_final_message_wo_proof},{client_proof_attr}={client_proof}",
        client_final_message_wo_proof = client_final_message_wo_proof,
        client_proof_attr = CLIENT_PROOF_ATTR,
        client_proof = base64::encode(&client_proof)
    );

    stream.send(SaslResponse(&client_final_message)).await?;

    let data = match stream.recv_expect(MessageFormat::Authentication).await? {
        Authentication::SaslFinal(data) => data,

        auth => {
            return Err(err_protocol!("expected SASLFinal but received {:?}", auth));
        }
    };

    // authentication is only considered valid if this verification passes
    mac.verify(&data.verifier).map_err(Error::protocol)?;

    Ok(())
}

// nonce is a sequence of random printable bytes
fn gen_nonce() -> String {
    let mut rng = rand::thread_rng();
    let count = rng.gen_range(64, 128);

    // printable = %x21-2B / %x2D-7E
    // ;; Printable ASCII except ",".
    // ;; Note that any "printable" is also
    // ;; a valid "value".
    let nonce: String = std::iter::repeat(())
        .map(|()| {
            let mut c = rng.gen_range(0x21, 0x7F) as u8;

            while c == 0x2C {
                c = rng.gen_range(0x21, 0x7F) as u8;
            }

            c
        })
        .take(count)
        .map(|c| c as char)
        .collect();

    rng.gen_range(32, 128);
    format!("{}={}", NONCE_ATTR, nonce)
}

// Hi(str, salt, i):
fn hi<'a>(s: &'a str, salt: &'a [u8], iter_count: u32) -> Result<[u8; 32], Error> {
    let mut mac = Hmac::<Sha256>::new_varkey(s.as_bytes()).map_err(Error::protocol)?;

    mac.input(&salt);
    mac.input(&1u32.to_be_bytes());

    let mut u = mac.result().code();
    let mut hi = u;

    for _ in 1..iter_count {
        let mut mac = Hmac::<Sha256>::new_varkey(s.as_bytes()).map_err(Error::protocol)?;
        mac.input(u.as_slice());
        u = mac.result().code();
        hi = hi.iter().zip(u.iter()).map(|(&a, &b)| a ^ b).collect();
    }

    Ok(hi.into())
}
