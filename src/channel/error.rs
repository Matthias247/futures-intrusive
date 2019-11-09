/// The error which is returned when sending a value into a channel fails.
///
/// The `send` operation can only fail if the channel has been closed, which
/// would prevent the other actors to ever retrieve the value.
///
/// The error recovers the value that has been sent.
#[derive(PartialEq, Debug)]
pub struct ChannelSendError<T>(pub T);
