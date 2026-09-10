// One analytic superellipse, approximated by tangent-matched cubic segments.
// This is a smooth optical fallback, not an export of Apple's private icon mask.
const exponent=5;
function point(t,x,y,size){const c=Math.cos(t),s=Math.sin(t),r=size/2/Math.pow(Math.abs(c)**exponent+Math.abs(s)**exponent,1/exponent);return [x+size/2+r*c,y+size/2+r*s];}
function derivative(t,x,y,size){const e=1e-5,a=point(t-e,x,y,size),b=point(t+e,x,y,size);return b.map((v,i)=>(v-a[i])/(2*e));}
function continuousPath(x,y,size){
 const n=128,h=2*Math.PI/n,f=p=>p.map(v=>v.toFixed(6)).join(' ');let d='M '+f(point(0,x,y,size));
 for(let i=0;i<n;i++){const a=i*h,b=(i+1)*h,p=point(a,x,y,size),q=point(b,x,y,size),u=derivative(a,x,y,size),v=derivative(b,x,y,size);d+=' C '+f(p.map((z,k)=>z+h*u[k]/3))+' '+f(q.map((z,k)=>z-h*v[k]/3))+' '+f(q);}
 return d+' Z';
}
function surface(theme='light'){
 const dark=theme==='dark',p=continuousPath(100,100,824),inner=continuousPath(105,105,814);
 return `<defs>
 <linearGradient id="porcelain" x1="0" y1="0" x2=".18" y2="1"><stop stop-color="${dark?'#424852':'#FFFFFF'}"/><stop offset=".38" stop-color="${dark?'#272C35':'#F5F5F6'}"/><stop offset=".83" stop-color="${dark?'#171B23':'#E9E8E6'}"/><stop offset="1" stop-color="${dark?'#292D35':'#D8D7D5'}"/></linearGradient>
 <radialGradient id="upper" cx=".4" cy="0" rx=".6" r=".85"><stop stop-color="white" stop-opacity="${dark?'.09':'.95'}"/><stop offset="1" stop-color="white" stop-opacity="0"/></radialGradient>
 <radialGradient id="lower" cx=".62" cy="1.03" r=".6"><stop stop-color="${dark?'#8B939F':'#FFFFFF'}" stop-opacity="${dark?'.3':'.94'}"/><stop offset=".45" stop-color="${dark?'#8B939F':'#FFFFFF'}" stop-opacity="${dark?'.07':'.32'}"/><stop offset="1" stop-color="white" stop-opacity="0"/></radialGradient>
 <linearGradient id="rim" x1=".25" y1="0" x2=".7" y2="1"><stop stop-color="white" stop-opacity=".96"/><stop offset=".35" stop-color="white" stop-opacity=".25"/><stop offset=".7" stop-color="white" stop-opacity=".05"/><stop offset="1" stop-color="white" stop-opacity=".95"/></linearGradient>
 <clipPath id="tileClip"><path d="${p}"/></clipPath>
 <filter id="softEdge" x="-20%" y="-20%" width="140%" height="140%"><feGaussianBlur stdDeviation="7"/></filter>
 </defs>
 <g clip-path="url(#tileClip)"><path d="${p}" fill="url(#porcelain)"/>
 <path d="${p}" fill="url(#upper)"/><path d="${p}" fill="url(#lower)"/>
 <path d="${p}" fill="none" stroke="${dark?'#080A0E':'#777D85'}" stroke-opacity="${dark?'.5':'.19'}" stroke-width="16" filter="url(#softEdge)"/>
 <path d="${p}" fill="none" stroke="${dark?'#AAB4C4':'#A9ABAD'}" stroke-opacity=".26" stroke-width="2"/>
 <path d="${inner}" fill="none" stroke="url(#rim)" stroke-width="4"/>
 </g>`;
}
module.exports={continuousPath,surface,point,derivative,exponent};
