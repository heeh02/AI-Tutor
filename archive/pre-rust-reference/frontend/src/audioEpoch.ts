export type AudioTicket = {epoch:number;sessionId:string|null};

/** Makes asynchronous audio responses conditional on the current local intent. */
export class AudioEpoch {
  private epoch=0;
  private sessionId:string|null=null;
  setSession(sessionId:string|null){if(this.sessionId!==sessionId){this.sessionId=sessionId;this.invalidate();}}
  invalidate(){this.epoch+=1;}
  capture():AudioTicket{return {epoch:this.epoch,sessionId:this.sessionId};}
  isCurrent(ticket:AudioTicket){return ticket.epoch===this.epoch&&ticket.sessionId===this.sessionId;}
}
